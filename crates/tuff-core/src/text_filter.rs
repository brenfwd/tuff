//! The external text filtering service: chat, signs and books are sent to an
//! operator-configured HTTP endpoint and come back with a mask of the
//! characters to hide. Off unless `text-filtering-config` is set. Version 0
//! authenticates with a static API key; version 1 signs in to Microsoft with
//! a client certificate and sends the bearer token it gets back. Every
//! answer is untrusted: bodies are size capped, requests are time bounded and
//! any failure hides the whole text, as the service being down must never let
//! unchecked text through.

mod safety;

use std::time::Duration;

use base64::Engine;
use reqwest::Url;
use serde_json::{Map, Value, json};

use crate::auth::GameProfile;
use tuff_storage::admin::format_uuid;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_READ_TIMEOUT_MS: u64 = 2000;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_CONCURRENT: usize = 64;
const DEFAULT_CONCURRENT: usize = 7;
const LEGACY_CHAT: &str = "v1/chat";

/// Which characters of a message the service wants hidden.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FilterMask {
    PassThrough,
    FullyFiltered,
    /// One flag per UTF-16 code unit of the message.
    Partial(Vec<bool>),
}

impl FilterMask {
    /// The text others get to see; `None` when it is hidden entirely.
    pub fn apply(&self, text: &str) -> Option<String> {
        match self {
            Self::PassThrough => Some(text.to_owned()),
            Self::FullyFiltered => None,
            Self::Partial(mask) => {
                let mut out = String::with_capacity(text.len());
                let mut unit = 0;
                for c in text.chars() {
                    let width = c.len_utf16();
                    let hidden =
                        (unit..unit + width).any(|i| mask.get(i).copied().unwrap_or(false));
                    out.push(if hidden { '#' } else { c });
                    unit += width;
                }
                Some(out)
            }
        }
    }

    pub fn is_pass_through(&self) -> bool {
        matches!(self, Self::PassThrough)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IgnoreStrategy {
    Never,
    FullyFiltered,
    Threshold(usize),
}

impl IgnoreStrategy {
    fn select(hashes_to_drop: i64) -> Self {
        match hashes_to_drop {
            -1 => Self::Never,
            0 => Self::FullyFiltered,
            n => Self::Threshold(usize::try_from(n).unwrap_or(usize::MAX)),
        }
    }

    fn should_ignore(self, message_units: usize, removed: usize) -> bool {
        match self {
            Self::Never => false,
            Self::FullyFiltered => message_units == removed,
            Self::Threshold(threshold) => removed >= threshold,
        }
    }
}

#[derive(Debug)]
enum Service {
    Legacy {
        join: Box<Url>,
        leave: Box<Url>,
        auth: String,
        rule_id: i64,
        legacy_chat: bool,
    },
    Safety(Box<safety::Safety>),
}

/// A configured, ready text filter.
#[derive(Debug)]
pub struct TextFilter {
    client: reqwest::Client,
    chat: Url,
    service: Service,
    server_id: String,
    room_id: String,
    ignore: IgnoreStrategy,
    slots: tokio::sync::Semaphore,
}

fn http_client(object: &Map<String, Value>) -> Result<reqwest::Client, String> {
    let read_ms = int(
        object,
        "connectionReadTimeoutMs",
        DEFAULT_READ_TIMEOUT_MS as i64,
    )
    .clamp(100, TOTAL_TIMEOUT.as_millis() as i64) as u64;
    reqwest::Client::builder()
        .tls_backend_preconfigured(crate::auth::tls_config().map_err(|e| e.to_string())?)
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(Duration::from_millis(read_ms))
        .timeout(TOTAL_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Minecraft server tuff")
        .build()
        .map_err(|e| e.to_string())
}

fn slots(object: &Map<String, Value>) -> tokio::sync::Semaphore {
    let concurrent = usize::try_from(int(
        object,
        "maxConcurrentRequests",
        DEFAULT_CONCURRENT as i64,
    ))
    .unwrap_or(DEFAULT_CONCURRENT)
    .clamp(1, MAX_CONCURRENT);
    tokio::sync::Semaphore::new(concurrent)
}

fn config_object(config: &str) -> Result<Map<String, Value>, String> {
    match serde_json::from_str(config).map_err(|e| format!("not valid JSON: {e}"))? {
        Value::Object(object) => Ok(object),
        _ => Err("the config must be a JSON object".into()),
    }
}

fn api_server(object: &Map<String, Value>) -> Result<Url, String> {
    let host =
        Url::parse(&string(object, "apiServer", None)?).map_err(|e| format!("apiServer: {e}"))?;
    if !matches!(host.scheme(), "http" | "https") {
        return Err("apiServer must be an http or https URL".into());
    }
    Ok(host)
}

async fn read_capped(mut response: reqwest::Response) -> Result<Value, String> {
    let mut buffer = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if buffer.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err("the response is too large".into());
        }
        buffer.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&buffer).map_err(|e| e.to_string())
}

impl TextFilter {
    /// Builds the filter from the two server.properties values. `Ok(None)`
    /// when no config is set; an error says why a set config is unusable.
    pub fn from_properties(config: &str, version: i64) -> Result<Option<Self>, String> {
        if config.trim().is_empty() {
            return Ok(None);
        }
        match version {
            0 => Self::from_legacy_config(config).map(Some),
            1 => Self::from_safety_config(config, safety::MICROSOFT_AUTHORITY).map(Some),
            other => Err(format!("unsupported text-filtering-version {other}")),
        }
    }

    fn from_legacy_config(config: &str) -> Result<Self, String> {
        let object = config_object(config)?;
        let host = api_server(&object)?;
        let key = string(&object, "apiKey", None)?;
        if key.is_empty() {
            return Err("Missing API key".into());
        }
        let endpoints = object.get("endpoints").and_then(Value::as_object);
        let endpoint = |id: &str, default: &str| -> Result<Url, String> {
            let path = endpoints
                .and_then(|e| e.get(id))
                .and_then(Value::as_str)
                .unwrap_or(default);
            host.join(&format!("/{path}"))
                .map_err(|e| format!("endpoint {id}: {e}"))
        };
        let chat_path = endpoints
            .and_then(|e| e.get("chat"))
            .and_then(Value::as_str)
            .unwrap_or(LEGACY_CHAT);
        Ok(Self {
            client: http_client(&object)?,
            chat: endpoint("chat", LEGACY_CHAT)?,
            service: Service::Legacy {
                join: Box::new(endpoint("join", "v1/join")?),
                leave: Box::new(endpoint("leave", "v1/leave")?),
                auth: format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(key.as_bytes())
                ),
                rule_id: int(&object, "ruleId", 1),
                legacy_chat: chat_path == LEGACY_CHAT,
            },
            server_id: string(&object, "serverId", Some("")).unwrap_or_default(),
            room_id: string(&object, "roomId", Some("Java:Chat")).unwrap_or_default(),
            ignore: IgnoreStrategy::select(int(&object, "hashesToDrop", -1)),
            slots: slots(&object),
        })
    }

    fn from_safety_config(config: &str, authority: &str) -> Result<Self, String> {
        let object = config_object(config)?;
        let host = api_server(&object)?;
        let chat = host
            .join(&string(&object, "apiPath", None)?)
            .map_err(|e| format!("apiPath: {e}"))?;
        let client = http_client(&object)?;
        Ok(Self {
            service: Service::Safety(Box::new(safety::Safety::from_config(&object, authority)?)),
            client,
            chat,
            server_id: string(&object, "serverId", Some("")).unwrap_or_default(),
            room_id: string(&object, "roomId", Some("Java:Chat")).unwrap_or_default(),
            ignore: IgnoreStrategy::select(int(&object, "hashesToDrop", -1)),
            slots: slots(&object),
        })
    }

    fn chat_body(&self, sender: &GameProfile, message: &str) -> Value {
        let id = format_uuid(sender.id);
        let (rule_id, legacy_chat) = match &self.service {
            Service::Legacy {
                rule_id,
                legacy_chat,
                ..
            } => (*rule_id, *legacy_chat),
            Service::Safety(_) => {
                return json!({
                    "userId": id, "userDisplayName": sender.name,
                    "server": self.server_id, "room": self.room_id,
                    "area": "JavaChatRealms", "data": message, "language": "*",
                });
            }
        };
        if legacy_chat {
            json!({
                "rule": rule_id, "server": self.server_id, "room": self.room_id,
                "player": id, "player_display_name": sender.name,
                "text": message, "language": "*",
            })
        } else {
            json!({
                "rule_id": rule_id.to_string(), "category": self.server_id,
                "subcategory": self.room_id, "user_id": id,
                "user_display_name": sender.name, "text": message, "language": "*",
            })
        }
    }

    fn membership_body(&self, player: &GameProfile) -> Value {
        json!({
            "server": self.server_id, "room": self.room_id,
            "user_id": format_uuid(player.id), "user_display_name": player.name,
        })
    }

    async fn post(&self, url: &Url, body: &Value) -> Result<Option<Value>, String> {
        let _slot = self.slots.acquire().await.map_err(|e| e.to_string())?;
        let auth = match &self.service {
            Service::Legacy { auth, .. } => auth.clone(),
            Service::Safety(safety) => safety.authorization(&self.client).await?,
        };
        let response = self
            .client
            .post(url.clone())
            .header("Authorization", auth)
            .header("Accept", "application/json")
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("the service answered {status}"));
        }
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(Some(Value::Object(Map::new())));
        }
        read_capped(response).await.map(Some)
    }

    /// Asks the service about one message. Never fails: an empty message
    /// passes, and any error hides the whole text.
    pub async fn filter(&self, sender: &GameProfile, message: &str) -> FilterMask {
        if message.is_empty() {
            return FilterMask::PassThrough;
        }
        let answer = self
            .post(&self.chat, &self.chat_body(sender, message))
            .await;
        match answer.and_then(|v| self.mask_from(message, v.unwrap_or(Value::Null))) {
            Ok(mask) => mask,
            Err(e) => {
                log::warn!("Failed to validate message from {}: {e}", sender.name);
                FilterMask::FullyFiltered
            }
        }
    }

    pub async fn player_joined(&self, player: &GameProfile) {
        if let Service::Legacy { join, .. } = &self.service {
            self.membership(join, player).await;
        }
    }

    pub async fn player_left(&self, player: &GameProfile) {
        if let Service::Legacy { leave, .. } = &self.service {
            self.membership(leave, player).await;
        }
    }

    async fn membership(&self, url: &Url, player: &GameProfile) {
        if let Err(e) = self.post(url, &self.membership_body(player)).await {
            log::warn!(
                "Failed to send join/leave to the text filter for {}: {e}",
                player.name
            );
        }
    }

    fn mask_from(&self, message: &str, response: Value) -> Result<FilterMask, String> {
        let object = response
            .as_object()
            .ok_or("the response is not an object")?;
        if let Service::Safety(safety) = &self.service {
            return safety.mask_from(object, |removed| self.parse_mask(message, removed));
        }
        if object
            .get("response")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Ok(FilterMask::PassThrough);
        }
        let Some(hashed) = object.get("hashed") else {
            return Ok(FilterMask::FullyFiltered);
        };
        hashed.as_str().ok_or("hashed is not a string")?;
        let removed = object
            .get("hashes")
            .and_then(Value::as_array)
            .ok_or("hashes is not an array")?;
        self.parse_mask(message, removed)
    }

    fn parse_mask(&self, message: &str, removed: &[Value]) -> Result<FilterMask, String> {
        if removed.is_empty() {
            return Ok(FilterMask::PassThrough);
        }
        let units = message.encode_utf16().count();
        if self.ignore.should_ignore(units, removed.len()) {
            return Ok(FilterMask::FullyFiltered);
        }
        let mut mask = vec![false; units];
        for index in removed {
            let index = index
                .as_u64()
                .ok_or("a filtered index is not a non-negative integer")?;
            if let Some(slot) = usize::try_from(index).ok().and_then(|i| mask.get_mut(i)) {
                *slot = true;
            }
        }
        Ok(FilterMask::Partial(mask))
    }
}

fn string(object: &Map<String, Value>, key: &str, default: Option<&str>) -> Result<String, String> {
    match (object.get(key), default) {
        (Some(Value::String(s)), _) => Ok(s.clone()),
        (None, Some(d)) => Ok(d.to_owned()),
        _ => Err(format!("{key} must be a string")),
    }
}

fn int(object: &Map<String, Value>, key: &str, default: i64) -> i64 {
    object.get(key).and_then(Value::as_i64).unwrap_or(default)
}

#[cfg(test)]
mod tests;
