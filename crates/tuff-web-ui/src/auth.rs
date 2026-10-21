//! Who may use the web panel: principals and scopes, one-time login codes,
//! sessions (persisted by hash in the server file area) and rate limits,
//! all owned by one actor.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rand::RngCore;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot, watch};

use tuff_storage::admin::PermissionLevel;
use tuff_storage::admin::{format_uuid, parse_uuid_text};
use tuff_storage::config::ConfigStore;

pub const WEB_ACCESS: &str = "tuff.web.access";
pub const WEB_LOGOUT_ALL: &str = "tuff.web.logout-all";
pub const WEB_METRICS_READ: &str = "tuff.web.metrics.read";
pub const WEB_LOGS_READ: &str = "tuff.web.logs.read";
pub const WEB_CONSOLE_RUN: &str = "tuff.web.console.run";
pub const WEB_PANEL_ADMIN: &str = "tuff.web.panel.admin";
pub const WEB_TIMINGS: &str = "tuff.web.timings";
pub const WEB_PERMISSIONS: &str = "tuff.web.permissions";
pub const WEB_BACKUPS_READ: &str = "tuff.web.backups.read";
pub const WEB_BACKUPS_MANAGE: &str = "tuff.web.backups.manage";
pub const WEB_BACKUPS_RESTORE: &str = "tuff.web.backups.restore";
pub const WEB_DATAPACKS_READ: &str = "tuff.web.datapacks.read";
pub const WEB_DATAPACKS_MANAGE: &str = "tuff.web.datapacks.manage";
pub const WEB_RESOURCEPACKS_READ: &str = "tuff.web.resourcepacks.read";
pub const WEB_RESOURCEPACKS_MANAGE: &str = "tuff.web.resourcepacks.manage";
pub const WEB_PLAYERS_READ: &str = "tuff.web.players.read";
pub const WEB_PLAYERS_MODERATE: &str = "tuff.web.players.moderate";
pub const WEB_CONFIG_READ: &str = "tuff.web.config.read";
pub const WEB_CONFIG_WRITE: &str = "tuff.web.config.write";
pub const WEB_SQL: &str = "tuff.web.sql";
pub const WEB_SQL_WRITE: &str = "tuff.web.sql.write";

/// The cookie holding a session id.
pub const COOKIE: &str = "tuff_web";
/// Where sessions are kept in the server file area.
pub const SESSIONS_FILE: &str = "web/sessions.json";
/// Where API tokens are kept, by hash of their secret.
pub const TOKENS_FILE: &str = "web/tokens.json";
/// How API tokens start: `tuff_<id>_<secret>`.
pub const TOKEN_PREFIX: &str = "tuff_";
const MAX_TOKENS: usize = 256;
const MAX_TOKEN_NAME: usize = 64;
const MAX_SESSIONS_FILE: usize = 4 << 20;
const MAX_SESSIONS: usize = 4096;
const MAX_CODES: usize = 1024;
const MAX_BUCKETS: usize = 16_384;
const PERSIST_LAST_SEEN: Duration = Duration::from_secs(300);
const REFUSALS_AUDITED: f64 = 5.0;

/// Who a session or command acts for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Principal {
    /// The server console, always owner level.
    Console,
    /// An operator, whose level is re-read on every request.
    Operator { uuid: u128, name: String },
    /// An API token, at the level it was created with.
    Token {
        id: String,
        name: String,
        level: PermissionLevel,
    },
}

impl Principal {
    pub fn name(&self) -> String {
        match self {
            Self::Console => "console".to_owned(),
            Self::Operator { name, .. } => name.clone(),
            Self::Token { name, .. } => format!("token:{name}"),
        }
    }

    pub fn to_json(&self) -> Value {
        match self {
            Self::Console => json!({ "kind": "console", "name": "console" }),
            Self::Operator { uuid, name } => {
                json!({ "kind": "operator", "uuid": format_uuid(*uuid), "name": name })
            }
            Self::Token { id, name, level } => {
                json!({ "kind": "token", "id": id, "name": name, "level": level.id() })
            }
        }
    }

    pub fn from_json(value: &Value) -> Option<Self> {
        match value.get("kind")?.as_str()? {
            "console" => Some(Self::Console),
            "operator" => Some(Self::Operator {
                uuid: parse_uuid_text(value.get("uuid")?.as_str()?)?,
                name: value.get("name")?.as_str()?.to_owned(),
            }),
            "token" => Some(Self::Token {
                id: value.get("id")?.as_str()?.to_owned(),
                name: value.get("name")?.as_str()?.to_owned(),
                level: PermissionLevel::from_id(value.get("level")?.as_i64()?)?,
            }),
            _ => None,
        }
    }
}

/// What a request may do. Every check goes through [`allows`], which
/// asks for the scope's permission node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    MetricsRead,
    LogsRead,
    ConsoleRun,
    PanelAdmin,
    /// Record timings and read their reports.
    Timings,
    /// Read and change permission settings and groups.
    Permissions,
    /// List and download backups, follow backup jobs.
    BackupsRead,
    /// Create, cancel, upload and delete backups.
    BackupsManage,
    /// Stage a restore and shut down to apply it.
    BackupsRestore,
    /// List and download data packs.
    DatapacksRead,
    /// Upload, enable, disable, remove and reload data packs.
    DatapacksManage,
    /// List and download resource packs and players' pack status.
    ResourcePacksRead,
    /// Upload, configure, push, pop and delete resource packs.
    ResourcePacksManage,
    /// See players and the operator, whitelist and ban lists.
    PlayersRead,
    /// Run moderation actions on players.
    PlayersModerate,
    /// See settings, server.properties and game rules.
    ConfigRead,
    /// Change settings, server.properties and game rules.
    ConfigWrite,
    /// Run read-only SQL against a SQLite world.
    SqlRead,
    /// Change a SQLite world with SQL (also needs the setting).
    SqlWrite,
    /// A scope a plugin names for its panel pages and APIs, namespaced by
    /// the plugin (`example.gold.read`).
    Plugin(&'static str),
}

impl Scope {
    pub const ALL: [Scope; 19] = [
        Scope::MetricsRead,
        Scope::LogsRead,
        Scope::ConsoleRun,
        Scope::PanelAdmin,
        Scope::Timings,
        Scope::Permissions,
        Scope::BackupsRead,
        Scope::BackupsManage,
        Scope::BackupsRestore,
        Scope::DatapacksRead,
        Scope::DatapacksManage,
        Scope::ResourcePacksRead,
        Scope::ResourcePacksManage,
        Scope::PlayersRead,
        Scope::PlayersModerate,
        Scope::ConfigRead,
        Scope::ConfigWrite,
        Scope::SqlRead,
        Scope::SqlWrite,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::MetricsRead => "metrics.read",
            Self::LogsRead => "logs.read",
            Self::ConsoleRun => "console.run",
            Self::PanelAdmin => "panel.admin",
            Self::Timings => "timings",
            Self::Permissions => "permissions",
            Self::BackupsRead => "backups.read",
            Self::BackupsManage => "backups.manage",
            Self::BackupsRestore => "backups.restore",
            Self::DatapacksRead => "datapacks.read",
            Self::DatapacksManage => "datapacks.manage",
            Self::ResourcePacksRead => "resourcepacks.read",
            Self::ResourcePacksManage => "resourcepacks.manage",
            Self::PlayersRead => "players.read",
            Self::PlayersModerate => "players.moderate",
            Self::ConfigRead => "config.read",
            Self::ConfigWrite => "config.write",
            Self::SqlRead => "sql.read",
            Self::SqlWrite => "sql.write",
            Self::Plugin(name) => name,
        }
    }
}

/// The permission node a scope needs; a plugin scope is its own node.
pub fn scope_node(scope: Scope) -> &'static str {
    match scope {
        Scope::MetricsRead => WEB_METRICS_READ,
        Scope::LogsRead => WEB_LOGS_READ,
        Scope::ConsoleRun => WEB_CONSOLE_RUN,
        Scope::PanelAdmin => WEB_PANEL_ADMIN,
        Scope::Timings => WEB_TIMINGS,
        Scope::Permissions => WEB_PERMISSIONS,
        Scope::BackupsRead => WEB_BACKUPS_READ,
        Scope::BackupsManage => WEB_BACKUPS_MANAGE,
        Scope::BackupsRestore => WEB_BACKUPS_RESTORE,
        Scope::DatapacksRead => WEB_DATAPACKS_READ,
        Scope::DatapacksManage => WEB_DATAPACKS_MANAGE,
        Scope::ResourcePacksRead => WEB_RESOURCEPACKS_READ,
        Scope::ResourcePacksManage => WEB_RESOURCEPACKS_MANAGE,
        Scope::PlayersRead => WEB_PLAYERS_READ,
        Scope::PlayersModerate => WEB_PLAYERS_MODERATE,
        Scope::ConfigRead => WEB_CONFIG_READ,
        Scope::ConfigWrite => WEB_CONFIG_WRITE,
        Scope::SqlRead => WEB_SQL,
        Scope::SqlWrite => WEB_SQL_WRITE,
        Scope::Plugin(name) => name,
    }
}

/// The operator level that holds a panel node by default: gamemasters
/// read logs and players, run commands and moderate; owners hold the rest.
pub fn node_level(node: &str) -> PermissionLevel {
    match node {
        WEB_LOGS_READ | WEB_CONSOLE_RUN | WEB_PLAYERS_READ | WEB_PLAYERS_MODERATE => {
            PermissionLevel::Gamemasters
        }
        _ => PermissionLevel::Owners,
    }
}

/// Every node at its default level, with nobody's settings.
#[derive(Debug)]
pub struct LevelDefaults;

impl PermissionCheck for LevelDefaults {
    fn allows(&self, principal: &Principal, level: PermissionLevel, node: &str) -> bool {
        matches!(principal, Principal::Console) || level >= node_level(node)
    }

    fn setting(&self, _: u128, _: &str) -> Option<bool> {
        None
    }
}

/// The server's permission settings, as a request sees them.
pub trait PermissionCheck: Send + Sync + std::fmt::Debug {
    /// Whether `principal` at `level` holds `node`: the console always
    /// does, an operator by their own or a role's setting, else by the
    /// node's default for `level`.
    fn allows(&self, principal: &Principal, level: PermissionLevel, node: &str) -> bool;
    /// A player's own or role setting of `node`, if any.
    fn setting(&self, uuid: u128, node: &str) -> Option<bool>;
}

/// Whether a principal at `level` holds `scope`'s node: by default
/// gamemasters and up read logs and run commands (at their own level),
/// owners read metrics, record timings, administer the panel and hold
/// every plugin scope a plugin did not register otherwise.
pub fn allows(
    view: &dyn PermissionCheck,
    principal: &Principal,
    level: PermissionLevel,
    scope: Scope,
) -> bool {
    view.allows(principal, level, scope_node(scope))
}

/// Whether a principal may hold a panel session: their own or a group's
/// setting of `tuff.web.access` decides, else operators at `min_level`
/// or above.
pub fn may_use(
    view: &dyn PermissionCheck,
    principal: &Principal,
    level: PermissionLevel,
    min_level: PermissionLevel,
) -> bool {
    match principal {
        Principal::Console | Principal::Token { .. } => true,
        Principal::Operator { uuid, .. } => view
            .setting(*uuid, WEB_ACCESS)
            .unwrap_or(level >= min_level),
    }
}

/// An authenticated request's identity, attached by the guard.
#[derive(Clone, Debug)]
pub struct AuthContext {
    pub principal: Principal,
    pub level: PermissionLevel,
    /// The session's id hash, which names it without revealing it.
    pub session: String,
    pub csrf: String,
    /// The permission snapshot taken for this request.
    pub permissions: Arc<dyn PermissionCheck>,
}

impl AuthContext {
    pub fn allows(&self, scope: Scope) -> bool {
        allows(&*self.permissions, &self.principal, self.level, scope)
    }

    /// A 403 response unless the context holds `scope`.
    pub fn require(&self, scope: Scope) -> Result<(), tuff_http::Response> {
        if self.allows(scope) {
            Ok(())
        } else {
            Err(super::guard::error(
                hyper::StatusCode::FORBIDDEN,
                "not allowed",
            ))
        }
    }

    pub fn scopes(&self) -> Vec<&'static str> {
        Scope::ALL
            .into_iter()
            .filter(|s| self.allows(*s))
            .map(Scope::name)
            .collect()
    }
}

/// Lifetimes and rates; tests shorten them.
#[derive(Clone, Debug)]
pub struct Limits {
    pub code_ttl: Duration,
    pub idle: Duration,
    pub absolute: Duration,
    /// Code exchanges per minute from one address.
    pub exchanges_per_ip: f64,
    /// Code exchanges per minute from everywhere.
    pub exchanges_total: f64,
    pub max_backoff: Duration,
    pub requests_per_sec: f64,
    pub request_burst: f64,
    pub commands_per_sec: f64,
    pub command_burst: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            code_ttl: Duration::from_secs(120),
            idle: Duration::from_secs(30 * 60),
            absolute: Duration::from_secs(12 * 3600),
            exchanges_per_ip: 10.0,
            exchanges_total: 60.0,
            max_backoff: Duration::from_secs(15 * 60),
            requests_per_sec: 20.0,
            request_burst: 60.0,
            commands_per_sec: 5.0,
            command_burst: 20.0,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExchangeError {
    RateLimited(Duration),
    Invalid,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Authenticated {
    Ok {
        principal: Principal,
        session: String,
        csrf: String,
    },
    RateLimited,
    Unknown,
    /// A bearer token that did not authenticate.
    Refused(TokenRefusal),
}

/// Why a bearer token was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenRefusal {
    /// The token the bearer named, when that id exists.
    pub token: Option<Principal>,
    pub reason: &'static str,
    /// Refusals from the same address since the last audited one; `None`
    /// while auditing them is throttled.
    pub audit: Option<u32>,
}

/// A new session: the secret id for the cookie and its context.
#[derive(Debug)]
pub struct NewSession {
    pub id: String,
    pub principal: Principal,
    pub session: String,
    pub csrf: String,
}

enum Request {
    NewCode {
        principal: Principal,
        reply: oneshot::Sender<String>,
    },
    Exchange {
        code: String,
        ip: IpAddr,
        reply: oneshot::Sender<Result<NewSession, ExchangeError>>,
    },
    Authenticate {
        id: String,
        reply: oneshot::Sender<Authenticated>,
    },
    CommandAllowed {
        session: String,
        reply: oneshot::Sender<bool>,
    },
    Logout {
        session: String,
    },
    LogoutPrincipal {
        principal: Principal,
        reply: oneshot::Sender<usize>,
    },
    LogoutAll {
        reply: oneshot::Sender<usize>,
    },
    Count {
        reply: oneshot::Sender<usize>,
    },
    #[cfg(test)]
    Panic,
    Flush {
        reply: oneshot::Sender<()>,
    },
    CreateToken {
        name: String,
        level: PermissionLevel,
        expires: Option<u64>,
        by: String,
        reply: oneshot::Sender<Option<NewToken>>,
    },
    Tokens {
        reply: oneshot::Sender<Vec<TokenInfo>>,
    },
    RevokeToken {
        id: String,
        reply: oneshot::Sender<bool>,
    },
    AuthenticateToken {
        bearer: String,
        ip: IpAddr,
        reply: oneshot::Sender<Authenticated>,
    },
    Sessions {
        reply: oneshot::Sender<Vec<SessionInfo>>,
    },
    RevokeSession {
        session: String,
        reply: oneshot::Sender<bool>,
    },
}

/// A token just made; `secret` is shown once and never kept.
#[derive(Clone, Debug)]
pub struct NewToken {
    pub principal: Principal,
    pub secret: String,
}

/// A token as listed: never its secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenInfo {
    pub id: String,
    pub name: String,
    pub level: PermissionLevel,
    pub created: u64,
    pub last_used: Option<u64>,
    /// Unix milliseconds after which it no longer authenticates.
    pub expires: Option<u64>,
    pub by: String,
}

impl TokenInfo {
    pub fn expired(&self) -> bool {
        self.expires.is_some_and(|at| unix_millis() >= at)
    }
}

/// A live session as listed, named by its id hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub session: String,
    pub principal: Principal,
    pub created: u64,
    pub last_seen: u64,
    pub remote: Option<String>,
}

/// Whether `name` may name a token: 1 to 64 letters, digits, `_`, `-`
/// or `.`.
pub fn valid_token_name(name: &str) -> bool {
    (1..=MAX_TOKEN_NAME).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// The id and secret of a `tuff_<id>_<secret>` bearer token.
fn split_token(bearer: &str) -> Option<(&str, &str)> {
    let (id, secret) = bearer.strip_prefix(TOKEN_PREFIX)?.split_once('_')?;
    let hex = |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit());
    (hex(id, 16) && hex(secret, 64)).then_some((id, secret))
}

/// `text` with the secret of every bearer token in it masked, so a token
/// shown on the server console never reaches panel log readers.
pub fn mask_tokens(text: &str) -> std::borrow::Cow<'_, str> {
    const SECRET_AT: usize = TOKEN_PREFIX.len() + 17;
    const LEN: usize = SECRET_AT + 64;
    if !text.contains(TOKEN_PREFIX) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(TOKEN_PREFIX) {
        let candidate = rest[at..].get(..LEN);
        match candidate.filter(|c| split_token(c).is_some()) {
            Some(token) => {
                out.push_str(&rest[..at]);
                out.push_str(&token[..SECRET_AT]);
                out.push_str("<redacted>");
                rest = &rest[at + LEN..];
            }
            None => {
                out.push_str(&rest[..at + TOKEN_PREFIX.len()]);
                rest = &rest[at + TOKEN_PREFIX.len()..];
            }
        }
    }
    out.push_str(rest);
    std::borrow::Cow::Owned(out)
}

/// The auth actor's handle.
#[derive(Clone)]
pub struct Auth {
    tx: mpsc::Sender<Request>,
}

impl Auth {
    /// Starts the actor, loading persisted sessions from `files`.
    pub async fn spawn(files: Option<Arc<dyn ConfigStore>>, limits: Limits) -> Auth {
        let loaded = match &files {
            Some(files) => {
                let files = files.clone();
                tokio::task::spawn_blocking(move || load(&*files))
                    .await
                    .unwrap_or_default()
            }
            None => HashMap::new(),
        };
        let tokens = match &files {
            Some(files) => {
                let files = files.clone();
                tokio::task::spawn_blocking(move || load_tokens(&*files))
                    .await
                    .unwrap_or_default()
            }
            None => HashMap::new(),
        };
        let (persist_tx, persist_rx) = watch::channel(None::<String>);
        let (tokens_tx, tokens_rx) = watch::channel(None::<String>);
        if let Some(files) = files {
            tuff_supervise::spawn(
                "the session saver",
                persist(files.clone(), SESSIONS_FILE, persist_rx),
            );
            tuff_supervise::spawn("the token saver", persist(files, TOKENS_FILE, tokens_rx));
        }
        let (tx, rx) = mpsc::channel(256);
        let actor = Actor {
            limits,
            codes: HashMap::new(),
            sessions: loaded,
            buckets: HashMap::new(),
            global: Bucket::full(0.0),
            session_buckets: HashMap::new(),
            command_buckets: HashMap::new(),
            persist: persist_tx,
            persisted_at: Instant::now(),
            tokens,
            persist_tokens: tokens_tx,
            tokens_saved_at: Instant::now(),
            token_refusals: HashMap::new(),
        };
        tuff_supervise::spawn("the panel auth actor", actor.restarting(rx));
        Auth { tx }
    }

    /// A handle nobody answers, for a `Shared` built without a runtime.
    pub fn detached() -> Auth {
        Auth {
            tx: mpsc::channel(1).0,
        }
    }

    async fn ask<T>(&self, make: impl FnOnce(oneshot::Sender<T>) -> Request) -> Option<T> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(make(reply)).await.ok()?;
        rx.await.ok()
    }

    /// A fresh single-use login code for `principal`, `XXXX-XXXX-XXXX-XXXX`.
    pub async fn new_code(&self, principal: Principal) -> Option<String> {
        self.ask(|reply| Request::NewCode { principal, reply })
            .await
    }

    pub async fn exchange(&self, code: &str, ip: IpAddr) -> Result<NewSession, ExchangeError> {
        let code = code.to_owned();
        self.ask(|reply| Request::Exchange { code, ip, reply })
            .await
            .unwrap_or(Err(ExchangeError::Invalid))
    }

    pub async fn authenticate(&self, id: &str) -> Authenticated {
        let id = id.to_owned();
        self.ask(|reply| Request::Authenticate { id, reply })
            .await
            .unwrap_or(Authenticated::Unknown)
    }

    /// Takes one token from the session's command bucket.
    pub async fn command_allowed(&self, session: &str) -> bool {
        let session = session.to_owned();
        self.ask(|reply| Request::CommandAllowed { session, reply })
            .await
            .unwrap_or(false)
    }

    pub async fn logout(&self, session: &str) {
        let _ = self
            .tx
            .send(Request::Logout {
                session: session.to_owned(),
            })
            .await;
    }

    /// Ends every session of `principal`; how many there were.
    pub async fn logout_principal(&self, principal: Principal) -> usize {
        self.ask(|reply| Request::LogoutPrincipal { principal, reply })
            .await
            .unwrap_or(0)
    }

    pub async fn logout_all(&self) -> usize {
        self.ask(|reply| Request::LogoutAll { reply })
            .await
            .unwrap_or(0)
    }

    pub async fn session_count(&self) -> usize {
        self.ask(|reply| Request::Count { reply })
            .await
            .unwrap_or(0)
    }

    /// Waits until every request before it is handled.
    pub async fn flush(&self) {
        let _ = self.ask(|reply| Request::Flush { reply }).await;
    }

    /// A new API token at `level`, valid until unix milliseconds
    /// `expires`; `None` when too many exist.
    pub async fn create_token(
        &self,
        name: String,
        level: PermissionLevel,
        expires: Option<u64>,
        by: String,
    ) -> Option<NewToken> {
        self.ask(|reply| Request::CreateToken {
            name,
            level,
            expires,
            by,
            reply,
        })
        .await
        .flatten()
    }

    pub async fn tokens(&self) -> Vec<TokenInfo> {
        self.ask(|reply| Request::Tokens { reply })
            .await
            .unwrap_or_default()
    }

    /// Deletes a token and ends the sessions it opened.
    pub async fn revoke_token(&self, id: &str) -> bool {
        let id = id.to_owned();
        self.ask(|reply| Request::RevokeToken { id, reply })
            .await
            .unwrap_or(false)
    }

    /// Checks an `Authorization: Bearer` token sent from `ip`.
    pub async fn authenticate_token(&self, bearer: &str, ip: IpAddr) -> Authenticated {
        let bearer = bearer.to_owned();
        self.ask(|reply| Request::AuthenticateToken { bearer, ip, reply })
            .await
            .unwrap_or(Authenticated::Unknown)
    }

    pub async fn sessions(&self) -> Vec<SessionInfo> {
        self.ask(|reply| Request::Sessions { reply })
            .await
            .unwrap_or_default()
    }

    /// Ends the session with id hash `session`.
    pub async fn revoke_session(&self, session: &str) -> bool {
        let session = session.to_owned();
        self.ask(|reply| Request::RevokeSession { session, reply })
            .await
            .unwrap_or(false)
    }
}

/// Hex SHA-256 of `text` with a purpose label.
fn digest(label: &str, text: &str) -> String {
    let mut h = Sha256::new();
    h.update(label.as_bytes());
    h.update([0]);
    h.update(text.as_bytes());
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The session's stored name: a hash of its secret id.
pub fn session_hash(id: &str) -> String {
    digest("tuff-session", id)
}

fn csrf_of(id: &str) -> String {
    digest("tuff-csrf", id)
}

/// Compares without leaking where two strings differ.
pub fn constant_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The 16 digits of a code as typed, without separators; `None` unless
/// exactly 16 digits.
pub fn normalize_code(code: &str) -> Option<String> {
    let digits: String = code.chars().filter(|c| !matches!(c, '-' | ' ')).collect();
    (digits.len() == 16 && digits.bytes().all(|b| b.is_ascii_digit())).then_some(digits)
}

fn new_code_digits() -> String {
    let mut rng = rand::rngs::OsRng;
    let mut out = String::with_capacity(16);
    while out.len() < 16 {
        let byte = (rng.next_u32() & 0xff) as u8;
        if byte < 250 {
            out.push(char::from(b'0' + byte % 10));
        }
    }
    out
}

pub fn group_code(digits: &str) -> String {
    digits
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("-")
}

#[derive(Clone, Debug)]
struct Session {
    principal: Principal,
    created: u64,
    last_seen: u64,
    /// The CSRF token is derived from the id, which only the browser
    /// holds; it is cached here once the browser shows the id.
    csrf: Option<String>,
    remote: Option<String>,
}

#[derive(Clone, Debug)]
struct Token {
    name: String,
    level: PermissionLevel,
    hash: String,
    created: u64,
    last_used: Option<u64>,
    expires: Option<u64>,
    by: String,
}

impl Token {
    fn live(&self, now: u64) -> bool {
        self.expires.is_none_or(|at| now < at)
    }
}

struct Code {
    principal: Principal,
    expires: Instant,
}

#[derive(Clone, Copy)]
struct Bucket {
    tokens: f64,
    at: Instant,
    failures: u32,
    blocked_until: Option<Instant>,
}

impl Bucket {
    fn full(tokens: f64) -> Self {
        Self {
            tokens,
            at: Instant::now(),
            failures: 0,
            blocked_until: None,
        }
    }

    fn take(&mut self, per_sec: f64, burst: f64) -> bool {
        let now = Instant::now();
        self.tokens =
            (self.tokens + now.duration_since(self.at).as_secs_f64() * per_sec).min(burst);
        self.at = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

struct Actor {
    limits: Limits,
    codes: HashMap<String, Code>,
    sessions: HashMap<String, Session>,
    buckets: HashMap<IpAddr, Bucket>,
    global: Bucket,
    session_buckets: HashMap<String, Bucket>,
    command_buckets: HashMap<String, Bucket>,
    persist: watch::Sender<Option<String>>,
    persisted_at: Instant,
    tokens: HashMap<String, Token>,
    persist_tokens: watch::Sender<Option<String>>,
    tokens_saved_at: Instant,
    token_refusals: HashMap<IpAddr, Bucket>,
}

impl Actor {
    async fn restarting(mut self, mut rx: mpsc::Receiver<Request>) {
        use tuff_supervise::{CatchUnwind, count_panic, panic_message};
        self.global = Bucket::full(self.limits.exchanges_total);
        while let Err(panic) = CatchUnwind::new(Box::pin(self.run(&mut rx))).await {
            count_panic();
            log::error!(
                "the panel auth actor panicked: {}; restarting it with its sessions and tokens",
                panic_message(&*panic)
            );
            self.codes.clear();
            self.buckets.clear();
            self.session_buckets.clear();
            self.command_buckets.clear();
            self.token_refusals.clear();
            self.global = Bucket::full(self.limits.exchanges_total);
        }
    }

    async fn run(&mut self, rx: &mut mpsc::Receiver<Request>) {
        while let Some(request) = rx.recv().await {
            match request {
                #[cfg(test)]
                Request::Panic => panic!("test panic"),
                Request::NewCode { principal, reply } => {
                    let _ = reply.send(self.new_code(principal));
                }
                Request::Exchange { code, ip, reply } => {
                    let _ = reply.send(self.exchange(&code, ip));
                }
                Request::Authenticate { id, reply } => {
                    let _ = reply.send(self.authenticate(&id));
                }
                Request::CommandAllowed { session, reply } => {
                    let (rate, burst) = (self.limits.commands_per_sec, self.limits.command_burst);
                    let bucket = self
                        .command_buckets
                        .entry(session)
                        .or_insert_with(|| Bucket::full(burst));
                    let _ = reply.send(bucket.take(rate, burst));
                }
                Request::Logout { session } => {
                    if self.sessions.remove(&session).is_some() {
                        self.save();
                    }
                }
                Request::LogoutPrincipal { principal, reply } => {
                    let before = self.sessions.len();
                    self.sessions.retain(|_, s| s.principal != principal);
                    let removed = before - self.sessions.len();
                    if removed > 0 {
                        self.save();
                    }
                    let _ = reply.send(removed);
                }
                Request::LogoutAll { reply } => {
                    let removed = self.sessions.len();
                    self.sessions.clear();
                    self.save();
                    let _ = reply.send(removed);
                }
                Request::Count { reply } => {
                    self.expire();
                    let _ = reply.send(self.sessions.len());
                }
                Request::Flush { reply } => {
                    let _ = reply.send(());
                }
                Request::CreateToken {
                    name,
                    level,
                    expires,
                    by,
                    reply,
                } => {
                    let _ = reply.send(self.create_token(name, level, expires, by));
                }
                Request::Tokens { reply } => {
                    let mut list: Vec<TokenInfo> = self
                        .tokens
                        .iter()
                        .map(|(id, t)| TokenInfo {
                            id: id.clone(),
                            name: t.name.clone(),
                            level: t.level,
                            created: t.created,
                            last_used: t.last_used,
                            expires: t.expires,
                            by: t.by.clone(),
                        })
                        .collect();
                    list.sort_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)));
                    let _ = reply.send(list);
                }
                Request::RevokeToken { id, reply } => {
                    let found = self.tokens.remove(&id).is_some();
                    if found {
                        self.save_tokens();
                        let before = self.sessions.len();
                        self.sessions.retain(
                            |_, s| !matches!(&s.principal, Principal::Token { id: t, .. } if *t == id),
                        );
                        if self.sessions.len() != before {
                            self.save();
                        }
                    }
                    let _ = reply.send(found);
                }
                Request::AuthenticateToken { bearer, ip, reply } => {
                    let _ = reply.send(self.authenticate_token(&bearer, ip));
                }
                Request::Sessions { reply } => {
                    self.expire();
                    let mut list: Vec<SessionInfo> = self
                        .sessions
                        .iter()
                        .map(|(hash, s)| SessionInfo {
                            session: hash.clone(),
                            principal: s.principal.clone(),
                            created: s.created,
                            last_seen: s.last_seen,
                            remote: s.remote.clone(),
                        })
                        .collect();
                    list.sort_by_key(|a| std::cmp::Reverse(a.last_seen));
                    let _ = reply.send(list);
                }
                Request::RevokeSession { session, reply } => {
                    let found = self.sessions.remove(&session).is_some();
                    if found {
                        self.save();
                    }
                    let _ = reply.send(found);
                }
            }
        }
    }

    fn new_code(&mut self, principal: Principal) -> String {
        let now = Instant::now();
        self.codes.retain(|_, c| c.expires > now);
        if self.codes.len() >= MAX_CODES {
            let oldest = self
                .codes
                .iter()
                .min_by_key(|(_, c)| c.expires)
                .map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                self.codes.remove(&oldest);
            }
        }
        let digits = loop {
            let digits = new_code_digits();
            if !self.codes.contains_key(&digits) {
                break digits;
            }
        };
        self.codes.insert(
            digits.clone(),
            Code {
                principal,
                expires: now + self.limits.code_ttl,
            },
        );
        group_code(&digits)
    }

    fn exchange(&mut self, code: &str, ip: IpAddr) -> Result<NewSession, ExchangeError> {
        let now = Instant::now();
        if self.buckets.len() >= MAX_BUCKETS {
            self.buckets.retain(|_, b| {
                now.duration_since(b.at) < Duration::from_secs(3600)
                    || b.blocked_until.is_some_and(|u| u > now)
            });
        }
        let per_ip = self.limits.exchanges_per_ip;
        let bucket = self
            .buckets
            .entry(ip)
            .or_insert_with(|| Bucket::full(per_ip));
        if let Some(until) = bucket.blocked_until.filter(|u| *u > now) {
            return Err(ExchangeError::RateLimited(until - now));
        }
        if !bucket.take(per_ip / 60.0, per_ip) {
            return Err(ExchangeError::RateLimited(Duration::from_secs(60)));
        }
        let total = self.limits.exchanges_total;
        if !self.global.take(total / 60.0, total) {
            return Err(ExchangeError::RateLimited(Duration::from_secs(10)));
        }
        self.codes.retain(|_, c| c.expires > now);
        let found = normalize_code(code).and_then(|digits| self.codes.remove(&digits));
        let bucket = self.buckets.get_mut(&ip).expect("bucket just made");
        let Some(found) = found else {
            bucket.failures = bucket.failures.saturating_add(1);
            let backoff = Duration::from_secs(1u64 << (bucket.failures - 1).min(20))
                .min(self.limits.max_backoff);
            bucket.blocked_until = Some(now + backoff);
            return Err(ExchangeError::Invalid);
        };
        bucket.failures = 0;
        bucket.blocked_until = None;
        let mut raw = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut raw);
        let id = hex(&raw);
        let session = session_hash(&id);
        let csrf = csrf_of(&id);
        self.expire();
        if self.sessions.len() >= MAX_SESSIONS {
            let oldest = self
                .sessions
                .iter()
                .min_by_key(|(_, s)| s.last_seen)
                .map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                self.sessions.remove(&oldest);
            }
        }
        let at = unix_millis();
        self.sessions.insert(
            session.clone(),
            Session {
                principal: found.principal.clone(),
                created: at,
                last_seen: at,
                csrf: Some(csrf.clone()),
                remote: Some(ip.to_string()),
            },
        );
        self.save();
        Ok(NewSession {
            id,
            principal: found.principal,
            session,
            csrf,
        })
    }

    fn expire(&mut self) {
        let now = unix_millis();
        let idle = self.limits.idle.as_millis() as u64;
        let absolute = self.limits.absolute.as_millis() as u64;
        let before = self.sessions.len();
        self.sessions.retain(|_, s| {
            now.saturating_sub(s.last_seen) < idle && now.saturating_sub(s.created) < absolute
        });
        if self.sessions.len() != before {
            self.session_buckets
                .retain(|k, _| self.sessions.contains_key(k));
            self.command_buckets
                .retain(|k, _| self.sessions.contains_key(k));
            self.save();
        }
    }

    fn authenticate(&mut self, id: &str) -> Authenticated {
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Authenticated::Unknown;
        }
        self.expire();
        let hash = session_hash(id);
        let dead_token = match self.sessions.get(&hash).map(|s| &s.principal) {
            None => return Authenticated::Unknown,
            Some(Principal::Token { id, .. }) => !self.token_live(id),
            Some(_) => false,
        };
        if dead_token {
            self.sessions.remove(&hash);
            self.save();
            return Authenticated::Unknown;
        }
        let Some(session) = self.sessions.get_mut(&hash) else {
            return Authenticated::Unknown;
        };
        session.last_seen = unix_millis();
        let csrf = session.csrf.get_or_insert_with(|| csrf_of(id)).clone();
        let principal = session.principal.clone();
        let (rate, burst) = (self.limits.requests_per_sec, self.limits.request_burst);
        let allowed = self
            .session_buckets
            .entry(hash.clone())
            .or_insert_with(|| Bucket::full(burst))
            .take(rate, burst);
        if self.persisted_at.elapsed() >= PERSIST_LAST_SEEN {
            self.save();
        }
        if !allowed {
            return Authenticated::RateLimited;
        }
        Authenticated::Ok {
            principal,
            session: hash,
            csrf,
        }
    }

    fn create_token(
        &mut self,
        name: String,
        level: PermissionLevel,
        expires: Option<u64>,
        by: String,
    ) -> Option<NewToken> {
        if self.tokens.len() >= MAX_TOKENS {
            let now = unix_millis();
            self.tokens.retain(|_, t| t.live(now));
        }
        if self.tokens.len() >= MAX_TOKENS {
            return None;
        }
        let mut raw = [0u8; 8];
        let id = loop {
            rand::rngs::OsRng.fill_bytes(&mut raw);
            let id = hex(&raw);
            if !self.tokens.contains_key(&id) {
                break id;
            }
        };
        let mut secret = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut secret);
        let secret = hex(&secret);
        self.tokens.insert(
            id.clone(),
            Token {
                name: name.clone(),
                level,
                hash: digest("tuff-token", &secret),
                created: unix_millis(),
                last_used: None,
                expires,
                by,
            },
        );
        self.save_tokens();
        Some(NewToken {
            secret: format!("{TOKEN_PREFIX}{id}_{secret}"),
            principal: Principal::Token { id, name, level },
        })
    }

    fn authenticate_token(&mut self, bearer: &str, ip: IpAddr) -> Authenticated {
        let Some((id, secret)) = split_token(bearer.trim()) else {
            return self.refuse(ip, None, "malformed token");
        };
        let Some(token) = self.tokens.get_mut(id) else {
            return self.refuse(ip, None, "unknown token");
        };
        let named = Principal::Token {
            id: id.to_owned(),
            name: token.name.clone(),
            level: token.level,
        };
        if !constant_eq(&token.hash, &digest("tuff-token", secret)) {
            return self.refuse(ip, Some(named), "wrong secret");
        }
        if !token.live(unix_millis()) {
            return self.refuse(ip, Some(named), "expired");
        }
        let first = token.last_used.is_none();
        token.last_used = Some(unix_millis());
        let principal = Principal::Token {
            id: id.to_owned(),
            name: token.name.clone(),
            level: token.level,
        };
        let session = format!("token:{id}");
        if first || self.tokens_saved_at.elapsed() >= PERSIST_LAST_SEEN {
            self.save_tokens();
        }
        let (rate, burst) = (self.limits.requests_per_sec, self.limits.request_burst);
        let allowed = self
            .session_buckets
            .entry(session.clone())
            .or_insert_with(|| Bucket::full(burst))
            .take(rate, burst);
        if !allowed {
            return Authenticated::RateLimited;
        }
        Authenticated::Ok {
            principal,
            session,
            csrf: String::new(),
        }
    }

    /// A refused bearer token, audited at most a few times a minute per
    /// address so a misbehaving script cannot flood the audit log.
    fn refuse(
        &mut self,
        ip: IpAddr,
        token: Option<Principal>,
        reason: &'static str,
    ) -> Authenticated {
        let now = Instant::now();
        if self.token_refusals.len() >= MAX_BUCKETS {
            self.token_refusals
                .retain(|_, b| now.duration_since(b.at) < Duration::from_secs(3600));
        }
        let bucket = self
            .token_refusals
            .entry(ip)
            .or_insert_with(|| Bucket::full(REFUSALS_AUDITED));
        let audit = if bucket.take(REFUSALS_AUDITED / 60.0, REFUSALS_AUDITED) {
            Some(std::mem::take(&mut bucket.failures))
        } else {
            bucket.failures = bucket.failures.saturating_add(1);
            None
        };
        Authenticated::Refused(TokenRefusal {
            token,
            reason,
            audit,
        })
    }

    fn token_live(&self, id: &str) -> bool {
        self.tokens.get(id).is_some_and(|t| t.live(unix_millis()))
    }

    fn save_tokens(&mut self) {
        self.tokens_saved_at = Instant::now();
        let list: Vec<Value> = self
            .tokens
            .iter()
            .map(|(id, t)| {
                json!({
                    "id": id,
                    "name": t.name,
                    "level": t.level.id(),
                    "hash": t.hash,
                    "created": t.created,
                    "lastUsed": t.last_used,
                    "expires": t.expires,
                    "by": t.by,
                })
            })
            .collect();
        self.persist_tokens
            .send_replace(Some(Value::Array(list).to_string()));
    }

    fn save(&mut self) {
        self.persisted_at = Instant::now();
        let list: Vec<Value> = self
            .sessions
            .iter()
            .map(|(hash, s)| {
                json!({
                    "hash": hash,
                    "principal": s.principal.to_json(),
                    "created": s.created,
                    "lastSeen": s.last_seen,
                    "remote": s.remote,
                })
            })
            .collect();
        self.persist
            .send_replace(Some(Value::Array(list).to_string()));
    }
}

fn load(files: &dyn ConfigStore) -> HashMap<String, Session> {
    let Ok(Some(bytes)) = files.read_file(SESSIONS_FILE, MAX_SESSIONS_FILE) else {
        return HashMap::new();
    };
    let Ok(Value::Array(list)) = serde_json::from_slice::<Value>(&bytes) else {
        return HashMap::new();
    };
    list.iter()
        .take(MAX_SESSIONS)
        .filter_map(|entry| {
            let hash = entry.get("hash")?.as_str()?;
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            Some((
                hash.to_owned(),
                Session {
                    principal: Principal::from_json(entry.get("principal")?)?,
                    created: entry.get("created")?.as_u64()?,
                    last_seen: entry.get("lastSeen")?.as_u64()?,
                    csrf: None,
                    remote: entry
                        .get("remote")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                },
            ))
        })
        .collect()
}

fn load_tokens(files: &dyn ConfigStore) -> HashMap<String, Token> {
    let Ok(Some(bytes)) = files.read_file(TOKENS_FILE, MAX_SESSIONS_FILE) else {
        return HashMap::new();
    };
    let Ok(Value::Array(list)) = serde_json::from_slice::<Value>(&bytes) else {
        return HashMap::new();
    };
    list.iter()
        .take(MAX_TOKENS)
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?;
            let hash = entry.get("hash")?.as_str()?;
            let hexed =
                |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit());
            if !hexed(id, 16) || !hexed(hash, 64) {
                return None;
            }
            let name = entry.get("name")?.as_str()?;
            if !valid_token_name(name) {
                return None;
            }
            Some((
                id.to_owned(),
                Token {
                    name: name.to_owned(),
                    level: PermissionLevel::from_id(entry.get("level")?.as_i64()?)?,
                    hash: hash.to_owned(),
                    created: entry.get("created")?.as_u64()?,
                    last_used: entry.get("lastUsed").and_then(Value::as_u64),
                    expires: entry.get("expires").and_then(Value::as_u64),
                    by: entry
                        .get("by")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                },
            ))
        })
        .collect()
}

/// Writes the latest snapshot to `file`, coalescing bursts.
async fn persist(
    files: Arc<dyn ConfigStore>,
    file: &'static str,
    mut rx: watch::Receiver<Option<String>>,
) {
    while rx.changed().await.is_ok() {
        let Some(text) = rx.borrow_and_update().clone() else {
            continue;
        };
        let files = files.clone();
        let written =
            tokio::task::spawn_blocking(move || files.write_file(file, text.as_bytes())).await;
        if let Ok(Err(err)) = written {
            log::warn!("cannot save {file}: {err}");
        }
    }
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
