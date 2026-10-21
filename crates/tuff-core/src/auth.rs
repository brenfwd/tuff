//! Online-mode player verification against the Mojang session server
//! (https://minecraft.wiki/w/Java_Edition_protocol/Encryption#Server).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::StatusCode;
use serde::{Deserialize, Deserializer};

pub const MOJANG_SESSION_SERVER: &str = "https://sessionserver.mojang.com";
pub const MOJANG_PROFILE_API: &str = "https://api.minecraftservices.com";
/// Most names one bulk profile lookup request may carry.
const LOOKUP_BATCH: usize = 10;
/// Longest name sent to the profile lookup; nothing longer can exist.
const MAX_LOOKUP_NAME: usize = 16;
/// Largest profile lookup response body read.
const MAX_LOOKUP_RESPONSE: usize = 64 * 1024;
/// Largest profile response body read.
const MAX_PROFILE_RESPONSE: usize = 256 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GameProfile {
    #[serde(deserialize_with = "deserialize_uuid")]
    pub id: u128,
    pub name: String,
    #[serde(default)]
    pub properties: Vec<ProfileProperty>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProfileProperty {
    pub name: String,
    pub value: String,
    pub signature: Option<String>,
}

fn deserialize_uuid<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u128, D::Error> {
    let raw = String::deserialize(deserializer)?;
    parse_uuid(&raw).ok_or_else(|| serde::de::Error::custom(format!("invalid UUID: {raw}")))
}

/// Parses a UUID in either undashed (session server) or dashed form.
pub fn parse_uuid(raw: &str) -> Option<u128> {
    let hex: String = raw.chars().filter(|&c| c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    u128::from_str_radix(&hex, 16).ok()
}

/// The name-based (version 3, MD5) UUID of `bytes`.
pub fn name_uuid(bytes: &[u8]) -> u128 {
    use md5::{Digest, Md5};
    let mut hash: [u8; 16] = Md5::digest(bytes).into();
    hash[6] = (hash[6] & 0x0F) | 0x30;
    hash[8] = (hash[8] & 0x3F) | 0x80;
    u128::from_be_bytes(hash)
}

/// The UUID vanilla assigns in offline mode: a version 3 (MD5, name-based)
/// UUID of `OfflinePlayer:<name>`.
pub fn offline_uuid(name: &str) -> u128 {
    name_uuid(format!("OfflinePlayer:{name}").as_bytes())
}

pub(crate) fn tls_config() -> Result<rustls::ClientConfig> {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Ok(
        rustls::ClientConfig::builder_with_provider(Arc::new(rustls_rustcrypto::provider()))
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .tls_backend_preconfigured(tls_config()?)
        .timeout(REQUEST_TIMEOUT)
        .build()?)
}

#[derive(Debug, Deserialize)]
struct FoundProfile {
    #[serde(deserialize_with = "deserialize_uuid")]
    id: u128,
    name: String,
}

/// Client for the profile API's bulk name to UUID lookup, which resolves
/// names of players who have never joined this server.
#[derive(Clone)]
pub struct ProfileLookup {
    client: reqwest::Client,
    base_url: Arc<str>,
    session_url: Arc<str>,
}

impl ProfileLookup {
    pub fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            client: http_client()?,
            base_url: base_url.trim_end_matches('/').into(),
            session_url: MOJANG_SESSION_SERVER.into(),
        })
    }

    /// Fetches profiles by id from `url` instead of the Mojang session server.
    pub fn with_session_server(mut self, url: &str) -> Self {
        self.session_url = url.trim_end_matches('/').into();
        self
    }

    /// The full profile, with its signed properties, of the account `id`,
    /// or `None` when no account has it.
    pub async fn profile_by_id(&self, id: u128) -> Result<Option<GameProfile>> {
        let mut response = self
            .client
            .get(format!(
                "{}/session/minecraft/profile/{id:032x}",
                self.session_url
            ))
            .query(&[("unsigned", "false")])
            .send()
            .await
            .context("profile request failed")?;
        match response.status() {
            StatusCode::OK => {}
            StatusCode::NO_CONTENT | StatusCode::NOT_FOUND => return Ok(None),
            status => bail!("session server returned {status}"),
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if body.len() + chunk.len() > MAX_PROFILE_RESPONSE {
                bail!("profile response too large");
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body)
            .map(Some)
            .context("invalid session server profile")
    }

    /// The `(name, uuid)` of every name that belongs to an account, with
    /// the account's own capitalization. Names the service does not know
    /// are left out.
    pub async fn by_names(&self, names: &[String]) -> Result<Vec<(String, u128)>> {
        let names: Vec<&str> = names
            .iter()
            .map(String::as_str)
            .filter(|name| !name.is_empty() && name.len() <= MAX_LOOKUP_NAME)
            .collect();
        let mut found = Vec::new();
        for batch in names.chunks(LOOKUP_BATCH) {
            let mut response = self
                .client
                .post(format!(
                    "{}/minecraft/profile/lookup/bulk/byname",
                    self.base_url
                ))
                .json(batch)
                .send()
                .await
                .context("profile lookup request failed")?;
            if response.status() != StatusCode::OK {
                bail!("profile lookup returned {}", response.status());
            }
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                if body.len() + chunk.len() > MAX_LOOKUP_RESPONSE {
                    bail!("profile lookup response too large");
                }
                body.extend_from_slice(&chunk);
            }
            let profiles: Vec<FoundProfile> =
                serde_json::from_slice(&body).context("invalid profile lookup response")?;
            found.extend(
                profiles
                    .into_iter()
                    .filter(|p| batch.iter().any(|n| n.eq_ignore_ascii_case(&p.name)))
                    .map(|p| (p.name, p.id)),
            );
        }
        Ok(found)
    }
}

/// Mojang's public keys for player certificates, from the profile API at
/// `base_url`.
pub async fn fetch_profile_authority_keys(base_url: &str) -> Result<Vec<rsa::RsaPublicKey>> {
    let mut response = http_client()?
        .get(format!("{}/publickeys", base_url.trim_end_matches('/')))
        .send()
        .await
        .context("public keys request failed")?;
    if response.status() != StatusCode::OK {
        bail!("profile API returned {}", response.status());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > MAX_PROFILE_RESPONSE {
            bail!("public keys response too large");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(crate::protocol::chat_session::parse_authority_keys(&body))
}

/// Client for the session server's `hasJoined` endpoint. Stateless and cheap
/// to clone; every clone shares one connection pool.
#[derive(Clone)]
pub struct SessionService {
    client: reqwest::Client,
    base_url: Arc<str>,
}

impl SessionService {
    pub fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            client: http_client()?,
            base_url: base_url.trim_end_matches('/').into(),
        })
    }

    /// Returns the authenticated profile, or `None` if the session server does
    /// not recognize the login (the client did not call `join` for this hash).
    pub async fn has_joined(
        &self,
        username: &str,
        server_hash: &str,
        ip: Option<std::net::IpAddr>,
    ) -> Result<Option<GameProfile>> {
        let ip = ip.map(|ip| ip.to_string());
        let mut query = vec![("username", username), ("serverId", server_hash)];
        if let Some(ip) = &ip {
            query.push(("ip", ip));
        }
        let response = self
            .client
            .get(format!("{}/session/minecraft/hasJoined", self.base_url))
            .query(&query)
            .send()
            .await
            .context("session server request failed")?;

        match response.status() {
            StatusCode::OK => Ok(Some(
                response
                    .json()
                    .await
                    .context("invalid session server profile")?,
            )),
            StatusCode::NO_CONTENT => Ok(None),
            status => bail!("session server returned {status}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn parses_session_server_profile() {
        let profile: GameProfile = serde_json::from_str(
            r#"{"id":"069a79f444e94726a5befca90e38aaf5","name":"Notch","properties":[{"name":"textures","value":"abc","signature":"sig"}]}"#,
        )
        .unwrap();
        assert_eq!(profile.id, 0x069a79f444e94726a5befca90e38aaf5);
        assert_eq!(profile.name, "Notch");
        assert_eq!(profile.properties[0].signature.as_deref(), Some("sig"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn parse_uuid_accepts_dashed_and_rejects_garbage() {
        assert_eq!(
            parse_uuid("069a79f4-44e9-4726-a5be-fca90e38aaf5"),
            Some(0x069a79f444e94726a5befca90e38aaf5)
        );
        assert_eq!(parse_uuid("not-a-uuid"), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn offline_uuid_matches_vanilla() {
        assert_eq!(
            offline_uuid("Notch"),
            parse_uuid("b50ad385-829d-3141-a216-7e7d7539ba7f").unwrap()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn session_service_builds_with_pure_rust_tls() {
        SessionService::new(MOJANG_SESSION_SERVER).unwrap();
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    #[ignore = "requires network access to sessionserver.mojang.com"]
    async fn mojang_session_server_rejects_unknown_join() {
        let session = SessionService::new(MOJANG_SESSION_SERVER).unwrap();
        let result = session
            .has_joined("Notch", "not-a-real-hash", None)
            .await
            .unwrap();
        assert_eq!(result, None);
    }
}
