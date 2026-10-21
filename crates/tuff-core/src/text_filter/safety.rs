//! Version 1: Microsoft client-credentials sign-in with a certificate. A
//! JWT assertion signed by the certificate's RSA key buys a bearer token,
//! which is cached until shortly before it expires.

use std::collections::HashSet;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use reqwest::Url;
use rsa::RsaPrivateKey;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::signature::{SignatureEncoding, Signer};
use serde_json::{Map, Value, json};
use sha1::Digest;

use super::{FilterMask, read_capped, string};

pub(super) const MICROSOFT_AUTHORITY: &str = "https://login.microsoftonline.com/";
const ASSERTION_LIFETIME_S: u64 = 600;
const REFRESH_MARGIN: Duration = Duration::from_secs(300);
const MAX_CERTIFICATE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, PartialEq)]
pub(super) struct Credential {
    key: RsaPrivateKey,
    chain: Vec<Vec<u8>>,
}

#[derive(Debug)]
pub(super) struct Safety {
    token_url: Url,
    application_id: String,
    scope: String,
    credential: Credential,
    fully_filtered_events: HashSet<String>,
    token: tokio::sync::Mutex<Option<(String, Instant)>>,
}

impl Safety {
    pub(super) fn from_config(
        object: &Map<String, Value>,
        authority: &str,
    ) -> Result<Self, String> {
        let scope = string(object, "scope", None)?;
        let application_id = string(object, "applicationId", None)?;
        let tenant_id = string(object, "tenantId", None)?;
        let path = string(object, "certificatePath", None)?;
        let password = string(object, "certificatePassword", Some(""))?;
        let fully_filtered_events = object
            .get("fullyFilteredEvents")
            .and_then(Value::as_array)
            .ok_or("fullyFilteredEvents must be an array")?
            .iter()
            .map(|e| {
                e.as_str()
                    .map(str::to_owned)
                    .ok_or("a filtered event is not a string")
            })
            .collect::<Result<_, _>>()?;
        if tenant_id.is_empty() || tenant_id.contains(['/', '?', '#']) {
            return Err("tenantId is not a valid tenant".into());
        }
        let token_url = Url::parse(authority)
            .and_then(|a| a.join(&format!("{tenant_id}/oauth2/v2.0/token")))
            .map_err(|e| format!("tenantId: {e}"))?;
        let credential = load_credential(std::path::Path::new(&path), &password)
            .map_err(|e| format!("Failed to open certificate file {path}: {e}"))?;
        Ok(Self {
            token_url,
            application_id,
            scope,
            credential,
            fully_filtered_events,
            token: tokio::sync::Mutex::new(None),
        })
    }

    pub(super) async fn authorization(&self, client: &reqwest::Client) -> Result<String, String> {
        let mut cached = self.token.lock().await;
        if let Some((token, until)) = cached.as_ref()
            && Instant::now() < *until
        {
            return Ok(format!("Bearer {token}"));
        }
        let form = [
            ("client_id", self.application_id.as_str()),
            ("scope", self.scope.as_str()),
            ("grant_type", "client_credentials"),
            (
                "client_assertion_type",
                "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
            ),
            ("client_assertion", &self.assertion()?),
        ];
        let body = form
            .iter()
            .map(|(k, v)| format!("{k}={}", form_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let response = client
            .post(self.token_url.clone())
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| format!("sign-in failed: {e}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("sign-in answered {status}"));
        }
        let answer = read_capped(response).await?;
        let token = answer
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_graphic()))
            .ok_or("sign-in returned no usable access_token")?
            .to_owned();
        let lifetime = answer
            .get("expires_in")
            .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
            .unwrap_or(0);
        let until = Instant::now() + Duration::from_secs(lifetime).saturating_sub(REFRESH_MARGIN);
        *cached = Some((token.clone(), until));
        Ok(format!("Bearer {token}"))
    }

    #[cfg(test)]
    pub(super) fn credential_public_key(&self) -> rsa::RsaPublicKey {
        self.credential.key.to_public_key()
    }

    fn assertion(&self) -> Result<String, String> {
        let leaf = self.credential.chain.first().ok_or("no certificate")?;
        let header = json!({
            "alg": "RS256",
            "typ": "JWT",
            "x5t": URL_SAFE_NO_PAD.encode(sha1::Sha1::digest(leaf)),
            "x5c": self.credential.chain.iter().map(|c| STANDARD.encode(c)).collect::<Vec<_>>(),
        });
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs();
        let claims = json!({
            "aud": self.token_url.as_str(),
            "iss": self.application_id,
            "sub": self.application_id,
            "jti": format!("{:032x}", rand::random::<u128>()),
            "iat": now,
            "nbf": now,
            "exp": now + ASSERTION_LIFETIME_S,
        });
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let signer = rsa::pkcs1v15::SigningKey::<sha2::Sha256>::new(self.credential.key.clone());
        let signature = signer.sign(signing_input.as_bytes()).to_vec();
        Ok(format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature)
        ))
    }

    pub(super) fn mask_from(
        &self,
        response: &Map<String, Value>,
        parse_mask: impl FnOnce(&[Value]) -> Result<FilterMask, String>,
    ) -> Result<FilterMask, String> {
        let Some(result) = response.get("result").and_then(Value::as_object) else {
            return Ok(FilterMask::FullyFiltered);
        };
        if !result
            .get("filtered")
            .and_then(Value::as_bool)
            .unwrap_or(true)
        {
            return Ok(FilterMask::PassThrough);
        }
        let events = result.get("events").and_then(Value::as_array);
        let fully = events.into_iter().flatten().any(|event| {
            event
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| self.fully_filtered_events.contains(id))
        });
        if fully {
            return Ok(FilterMask::FullyFiltered);
        }
        match result.get("redactedTextIndex") {
            None => parse_mask(&[]),
            Some(Value::Array(removed)) => parse_mask(removed),
            Some(_) => Err("redactedTextIndex is not an array".into()),
        }
    }
}

fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Reads a PKCS#12 archive, or a PEM file holding the certificate chain
/// (leaf first) and an RSA private key.
pub(super) fn load_credential(
    path: &std::path::Path,
    password: &str,
) -> Result<Credential, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut data = Vec::new();
    file.take(MAX_CERTIFICATE_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() as u64 > MAX_CERTIFICATE_BYTES {
        return Err("the certificate file is too large".into());
    }
    match std::str::from_utf8(&data) {
        Ok(text) if text.contains("-----BEGIN ") => credential_from_pem(text),
        _ => credential_from_pkcs12(&data, password),
    }
}

fn credential_from_pkcs12(data: &[u8], password: &str) -> Result<Credential, String> {
    let store = p12_keystore::KeyStore::from_pkcs12(
        data,
        password,
        p12_keystore::Pkcs12ImportPolicy::Relaxed,
    )
    .map_err(|e| format!("not a readable PKCS#12 archive: {e}"))?;
    let (_, chain) = store
        .private_key_chain()
        .ok_or("the archive has no private key")?;
    let key = RsaPrivateKey::from_pkcs8_der(chain.key().as_der())
        .map_err(|e| format!("the private key is not RSA: {e}"))?;
    let chain: Vec<Vec<u8>> = chain.certs().iter().map(|c| c.as_der().to_vec()).collect();
    if chain.is_empty() {
        return Err("the archive has no certificate for its key".into());
    }
    Ok(Credential { key, chain })
}

pub(super) fn credential_from_pem(text: &str) -> Result<Credential, String> {
    let mut chain = Vec::new();
    let mut key = None;
    let mut rest = text;
    while let Some(begin) = rest.find("-----BEGIN ") {
        let after = &rest[begin + 11..];
        let label_end = after.find("-----").ok_or("a PEM header is not closed")?;
        let label = &after[..label_end];
        let end_marker = format!("-----END {label}-----");
        let end = after
            .find(&end_marker)
            .ok_or_else(|| format!("{label} has no END line"))?;
        let block = &rest[begin..begin + 11 + end + end_marker.len()];
        match label {
            "CERTIFICATE" => {
                let body: String = after[label_end + 5..end]
                    .chars()
                    .filter(|c| !c.is_ascii_whitespace())
                    .collect();
                chain.push(
                    STANDARD
                        .decode(body)
                        .map_err(|e| format!("CERTIFICATE: {e}"))?,
                );
            }
            "PRIVATE KEY" => {
                key = Some(
                    RsaPrivateKey::from_pkcs8_pem(block)
                        .map_err(|e| format!("PRIVATE KEY: {e}"))?,
                );
            }
            "RSA PRIVATE KEY" => {
                key = Some(
                    RsaPrivateKey::from_pkcs1_pem(block)
                        .map_err(|e| format!("RSA PRIVATE KEY: {e}"))?,
                );
            }
            _ => {}
        }
        rest = &after[end + end_marker.len()..];
    }
    let key = key.ok_or("no RSA private key in the PEM file")?;
    if chain.is_empty() {
        return Err("no certificate in the PEM file".into());
    }
    Ok(Credential { key, chain })
}
