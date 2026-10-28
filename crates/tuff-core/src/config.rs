use std::collections::BTreeMap;
use std::fmt;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    String,
    Int,
    Bool,
    Url,
    Ip,
    Port,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    String(String),
    Int(i64),
    Bool(bool),
    Url(String),
    Ip(IpAddr),
    Port(u16),
}

impl Value {
    pub fn kind(&self) -> Kind {
        match self {
            Value::String(_) => Kind::String,
            Value::Int(_) => Kind::Int,
            Value::Bool(_) => Kind::Bool,
            Value::Url(_) => Kind::Url,
            Value::Ip(_) => Kind::Ip,
            Value::Port(_) => Kind::Port,
        }
    }

    pub fn parse(kind: Kind, text: &str) -> Result<Value, ConfigError> {
        let bad = |what: &str| ConfigError::Invalid(format!("'{text}' is not {what}"));
        match kind {
            Kind::String => Ok(Value::String(text.to_owned())),
            Kind::Int => text
                .trim()
                .parse()
                .map(Value::Int)
                .map_err(|_| bad("an integer")),
            Kind::Bool => match text.trim() {
                "true" => Ok(Value::Bool(true)),
                "false" => Ok(Value::Bool(false)),
                _ => Err(bad("a boolean")),
            },
            Kind::Url => {
                if valid_url(text) {
                    Ok(Value::Url(text.to_owned()))
                } else {
                    Err(bad("a url"))
                }
            }
            Kind::Ip => text
                .trim()
                .parse()
                .map(Value::Ip)
                .map_err(|_| bad("an ip address")),
            Kind::Port => text
                .trim()
                .parse()
                .map(Value::Port)
                .map_err(|_| bad("a port")),
        }
    }
}

fn valid_url(text: &str) -> bool {
    let Some((scheme, rest)) = text.split_once("://") else {
        return false;
    };
    !scheme.is_empty()
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !rest.is_empty()
        && !text.chars().any(|c| c.is_whitespace() || c.is_control())
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::String(s) | Value::Url(s) => f.write_str(s),
            Value::Int(i) => write!(f, "{i}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Ip(ip) => write!(f, "{ip}"),
            Value::Port(p) => write!(f, "{p}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    Unknown(String),
    Duplicate(String),
    BadName(String),
    WrongKind { key: String, expected: Kind },
    Invalid(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Unknown(k) => write!(f, "unknown config key '{k}'"),
            ConfigError::Duplicate(k) => write!(f, "config key '{k}' already registered"),
            ConfigError::BadName(k) => write!(f, "invalid config key name '{k}'"),
            ConfigError::WrongKind { key, expected } => {
                write!(f, "config key '{key}' expects {expected:?}")
            }
            ConfigError::Invalid(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ConfigError {}

pub type Validator = fn(&Value) -> Result<(), String>;

#[derive(Clone)]
pub struct Key {
    pub name: String,
    pub default: Value,
    pub description: String,
    pub saved: bool,
    pub validator: Option<Validator>,
    pub hints: &'static [&'static str],
    /// A write-only value: shown only as set or unset and redacted from
    /// audit entries and command echoes.
    pub secret: bool,
    /// A string holding a MiniMessage text component.
    pub text_component: bool,
    /// The feature that registered the key; `None` for core's own.
    pub feature: Option<&'static str>,
}

impl Key {
    pub fn new(name: &str, default: Value, description: &str) -> Key {
        Key {
            name: name.to_owned(),
            default,
            description: description.to_owned(),
            saved: true,
            validator: None,
            hints: &[],
            secret: false,
            text_component: false,
            feature: None,
        }
    }

    pub fn secret(mut self) -> Key {
        self.secret = true;
        self
    }

    pub fn text_component(mut self) -> Key {
        self.text_component = true;
        self
    }

    pub fn hinted(mut self, hints: &'static [&'static str]) -> Key {
        self.hints = hints;
        self
    }

    pub fn suggestions(&self) -> Vec<String> {
        match self.kind() {
            Kind::Bool => vec!["true".to_owned(), "false".to_owned()],
            _ => self.hints.iter().map(|h| (*h).to_owned()).collect(),
        }
    }

    pub fn runtime_only(mut self) -> Key {
        self.saved = false;
        self
    }

    pub fn validated(mut self, validator: Validator) -> Key {
        self.validator = Some(validator);
        self
    }

    pub fn kind(&self) -> Kind {
        self.default.kind()
    }

    fn check(&self, value: &Value) -> Result<(), ConfigError> {
        if value.kind() != self.kind() {
            return Err(ConfigError::WrongKind {
                key: self.name.clone(),
                expected: self.kind(),
            });
        }
        match self.validator {
            Some(v) => v(value).map_err(ConfigError::Invalid),
            None => Ok(()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub seq: u64,
    pub key: String,
    pub value: Value,
}

struct Entry {
    key: Key,
    value: Value,
    stored: Value,
}

pub type Sink = Box<dyn Fn(Vec<(String, String)>) + Send + Sync>;

struct Inner {
    entries: BTreeMap<String, Entry>,
    /// Saved text of keys nobody has registered (yet, or any more), kept
    /// so a feature loaded later finds its settings again.
    orphans: BTreeMap<String, String>,
    seq: u64,
    sink: Option<Sink>,
}

#[derive(Clone)]
pub struct ConfigStore {
    inner: Arc<Mutex<Inner>>,
    tx: Arc<watch::Sender<Option<Change>>>,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('.').all(|seg| {
            !seg.is_empty()
                && seg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

impl Default for ConfigStore {
    fn default() -> Self {
        ConfigStore::new()
    }
}

impl ConfigStore {
    pub fn empty() -> ConfigStore {
        let (tx, _) = watch::channel(None);
        ConfigStore {
            inner: Arc::new(Mutex::new(Inner {
                entries: BTreeMap::new(),
                orphans: BTreeMap::new(),
                seq: 0,
                sink: None,
            })),
            tx: Arc::new(tx),
        }
    }

    pub fn new() -> ConfigStore {
        let store = ConfigStore::empty();
        for key in core_keys() {
            store.register(key).expect("core keys are valid");
        }
        store
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn register(&self, key: Key) -> Result<(), ConfigError> {
        if !valid_name(&key.name) {
            return Err(ConfigError::BadName(key.name));
        }
        key.check(&key.default)?;
        let mut inner = self.lock();
        if inner.entries.contains_key(&key.name) {
            return Err(ConfigError::Duplicate(key.name));
        }
        let mut value = key.default.clone();
        if let Some(text) = inner.orphans.remove(&key.name) {
            match Value::parse(key.kind(), &text)
                .and_then(|saved| key.check(&saved).map(|()| saved))
            {
                Ok(saved) => value = saved,
                Err(err) => log::warn!("dropping saved setting {}: {err}", key.name),
            }
        }
        inner.entries.insert(
            key.name.clone(),
            Entry {
                key,
                stored: value.clone(),
                value,
            },
        );
        Ok(())
    }

    /// Removes a key a feature registered at runtime. A saved value stays
    /// in the saved settings and comes back when the key is registered
    /// again.
    pub fn unregister(&self, name: &str) -> bool {
        let mut inner = self.lock();
        let Some(entry) = inner.entries.remove(name) else {
            return false;
        };
        if entry.key.saved && entry.stored != entry.key.default {
            inner
                .orphans
                .insert(name.to_owned(), entry.stored.to_string());
        }
        true
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        self.lock().entries.get(name).map(|e| e.value.clone())
    }

    pub fn definition(&self, name: &str) -> Option<Key> {
        self.lock().entries.get(name).map(|e| e.key.clone())
    }

    /// Every key with its live value, by name.
    pub fn entries(&self) -> Vec<(Key, Value)> {
        self.lock()
            .entries
            .values()
            .map(|e| (e.key.clone(), e.value.clone()))
            .collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.lock().entries.keys().cloned().collect()
    }

    pub fn get_string(&self, name: &str) -> Option<String> {
        match self.get(name)? {
            Value::String(s) | Value::Url(s) => Some(s),
            _ => None,
        }
    }

    pub fn get_int(&self, name: &str) -> Option<i64> {
        match self.get(name)? {
            Value::Int(i) => Some(i),
            _ => None,
        }
    }

    pub fn sqlite_entity_index(&self) -> bool {
        self.get_bool(STORAGE_SQLITE_ENTITY_INDEX) == Some(true)
    }

    pub fn get_bool(&self, name: &str) -> Option<bool> {
        match self.get(name)? {
            Value::Bool(b) => Some(b),
            _ => None,
        }
    }

    pub fn get_ip(&self, name: &str) -> Option<IpAddr> {
        match self.get(name)? {
            Value::Ip(i) => Some(i),
            _ => None,
        }
    }

    pub fn get_port(&self, name: &str) -> Option<u16> {
        match self.get(name)? {
            Value::Port(p) => Some(p),
            _ => None,
        }
    }

    pub fn set(&self, name: &str, value: Value) -> Result<(), ConfigError> {
        self.apply(name, value, true)
    }

    /// Sets a value for this run only: it is never written back to the
    /// saved settings until the key is set normally.
    pub fn set_for_run(&self, name: &str, value: Value) -> Result<(), ConfigError> {
        self.apply(name, value, false)
    }

    fn apply(&self, name: &str, value: Value, persist: bool) -> Result<(), ConfigError> {
        let mut inner = self.lock();
        let entry = inner
            .entries
            .get(name)
            .ok_or_else(|| ConfigError::Unknown(name.to_owned()))?;
        entry.key.check(&value)?;
        let live_changed = entry.value != value;
        let stored_changed = persist && entry.stored != value;
        let saved = entry.key.saved;
        if !live_changed && !stored_changed {
            return Ok(());
        }
        let entry = inner.entries.get_mut(name).expect("checked");
        entry.value = value.clone();
        if persist {
            entry.stored = value.clone();
        }
        if live_changed {
            inner.seq += 1;
            let seq = inner.seq;
            self.tx.send_replace(Some(Change {
                seq,
                key: name.to_owned(),
                value,
            }));
        }
        if let (true, Some(sink)) = (persist && saved && stored_changed, &inner.sink) {
            sink(snapshot(&inner));
        }
        Ok(())
    }

    pub fn set_text(&self, name: &str, text: &str) -> Result<(), ConfigError> {
        self.set(name, self.parse_for(name, text)?)
    }

    pub fn set_text_for_run(&self, name: &str, text: &str) -> Result<(), ConfigError> {
        self.set_for_run(name, self.parse_for(name, text)?)
    }

    fn parse_for(&self, name: &str, text: &str) -> Result<Value, ConfigError> {
        let kind = self
            .definition(name)
            .ok_or_else(|| ConfigError::Unknown(name.to_owned()))?
            .kind();
        Value::parse(kind, text)
    }

    /// Installs what receives the full saved settings, as key and text
    /// pairs, after every change to a saved key. It runs under the
    /// store's lock, in change order, and must not call back into it.
    pub fn set_sink(&self, sink: Sink) {
        self.lock().sink = Some(sink);
    }

    /// The saved settings as key and text pairs.
    pub fn saved_text(&self) -> Vec<(String, String)> {
        snapshot(&self.lock())
    }

    pub fn reset(&self, name: &str) -> Result<(), ConfigError> {
        let default = self
            .definition(name)
            .ok_or_else(|| ConfigError::Unknown(name.to_owned()))?
            .default;
        self.set(name, default)
    }

    pub fn subscribe(&self) -> watch::Receiver<Option<Change>> {
        self.tx.subscribe()
    }

    pub fn saved(&self) -> Vec<(String, Value)> {
        self.lock()
            .entries
            .values()
            .filter(|e| e.key.saved && e.stored != e.key.default)
            .map(|e| (e.key.name.clone(), e.stored.clone()))
            .collect()
    }

    pub fn load_saved<'a>(
        &self,
        pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Vec<(String, ConfigError)> {
        let mut errors = Vec::new();
        for (name, text) in pairs {
            let saved = self.definition(name).map(|k| k.saved);
            let result = match saved {
                Some(true) => self.set_text(name, text),
                Some(false) => Ok(()),
                None => {
                    self.lock().orphans.insert(name.to_owned(), text.to_owned());
                    Err(ConfigError::Unknown(name.to_owned()))
                }
            };
            if let Err(e) = result {
                errors.push((name.to_owned(), e));
            }
        }
        errors
    }
}

fn snapshot(inner: &Inner) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = inner
        .entries
        .values()
        .filter(|e| e.key.saved && e.stored != e.key.default)
        .map(|e| (e.key.name.clone(), e.stored.to_string()))
        .chain(inner.orphans.iter().map(|(k, v)| (k.clone(), v.clone())))
        .collect();
    pairs.sort();
    pairs
}

impl fmt::Debug for ConfigStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigStore")
            .field("keys", &self.names().len())
            .finish()
    }
}

pub const EXECUTE_ALLOW_OUTSIDE: &str = "tuff.execute.allowOutsideWorkingDirectory";
pub const MAX_OUTBOUND_BYTES: &str = "tuff.net.maxOutboundBytes";
pub const DEFAULT_MAX_OUTBOUND_BYTES: i64 = 32 * 1024 * 1024;
pub const HTTP_HOST: &str = "tuff.http.host";
pub const HTTP_PORT: &str = "tuff.http.port";
pub const HTTP_RESOURCE_BASE_URL: &str = "tuff.http.resourcePathBaseUrl";
pub const HTTP_DETECT_PUBLIC_IP: &str = "tuff.http.detectPublicIp";
pub const STORAGE_SQLITE_ENTITY_INDEX: &str = "tuff.storage.sqlite.entityIndex";

pub const OFFLINE_OPS: &str = "tuff.offlineOps";
pub const ALLOW_PUBLIC_OFFLINE: &str = "tuff.allowPublicOffline";
pub const REFUSE_DUPLICATE_LOGIN: &str = "tuff.refuseDuplicateLogin";
pub const ITEMS_CONSERVATION_CHECKS: &str = "tuff.items.conservationChecks";
pub const ITEMS_CONSERVATION_KICK: &str = "tuff.items.conservationKick";
pub const BLOCKS_TNT_DUPLICATION: &str = "tuff.blocks.tntDuplication";
pub const BLOCKS_PERMANENT_BLOCK_BREAKING: &str = "tuff.blocks.permanentBlockBreaking";
pub const BLOCKS_CARPET_RAIL_DUPLICATION: &str = "tuff.blocks.carpetRailDuplication";
pub const BLOCKS_GRAVITY_PORTAL_DUPLICATION: &str = "tuff.blocks.gravityBlockPortalDuplication";
fn core_keys() -> Vec<Key> {
    vec![
        Key::new(
            ITEMS_CONSERVATION_CHECKS,
            Value::Bool(true),
            "Check item moves between players, containers and regions in release builds: an operation that would make or lose items is refused, logged and audited, and the actors involved are not saved until they reload from their last consistent save",
        ),
        Key::new(
            BLOCKS_TNT_DUPLICATION,
            Value::Bool(true),
            "Keep vanilla's TNT duplication: TNT a piston moves can also light the copy left behind, once, through the one named door that makes the extra primed TNT",
        ),
        Key::new(
            BLOCKS_PERMANENT_BLOCK_BREAKING,
            Value::Bool(true),
            "Keep vanilla's permanent block breaking: a piston retraction that outlived its piston can still remove the unbreakable block (bedrock, End portal frame, command block) in front of the piston",
        ),
        Key::new(
            BLOCKS_CARPET_RAIL_DUPLICATION,
            Value::Bool(true),
            "Keep vanilla's carpet and rail duplication: a carpet or rail a piston moves can also pop where it stood, once, through the one named door that makes the extra drop",
        ),
        Key::new(
            BLOCKS_GRAVITY_PORTAL_DUPLICATION,
            Value::Bool(true),
            "Keep vanilla's gravity block duplication through End portals, through the one named door that makes the copy",
        ),
        Key::new(
            ITEMS_CONSERVATION_KICK,
            Value::Bool(true),
            "Disconnect a player involved in a refused item operation so the client reloads from the server's last consistent save",
        ),
        Key::new(
            OFFLINE_OPS,
            Value::Bool(false),
            "Honour ops.json for players whose names are not verified (offline mode without proxy forwarding); anyone can claim an operator's name",
        ),
        Key::new(
            ALLOW_PUBLIC_OFFLINE,
            Value::Bool(false),
            "Start in offline mode on a non-loopback address without proxy forwarding (anyone can join under any name); read at startup",
        ),
        Key::new(
            REFUSE_DUPLICATE_LOGIN,
            Value::Bool(false),
            "Refuse a login whose player is already online instead of kicking the online player",
        ),
        Key::new(
            crate::net::velocity::ENABLED_KEY,
            Value::Bool(false),
            "Accept players only through a Velocity proxy with modern forwarding, taking their identity and address from it; read at startup",
        ),
        Key::new(
            crate::net::velocity::SECRET_KEY,
            Value::String(String::new()),
            "The forwarding secret shared with the Velocity proxy; read at startup",
        )
        .secret(),
        Key::new(
            crate::net::velocity::SECRET_FILE_KEY,
            Value::String(String::new()),
            "File holding the Velocity forwarding secret (like Velocity's forwarding.secret), used when the secret key is empty; read at startup",
        ),
        Key::new(
            EXECUTE_ALLOW_OUTSIDE,
            Value::Bool(false),
            "Let /tuff execute read files outside the working directory",
        )
        .runtime_only(),
        Key::new(
            MAX_OUTBOUND_BYTES,
            Value::Int(DEFAULT_MAX_OUTBOUND_BYTES),
            "Bytes of packets queued for one client before it is disconnected for not reading (minimum 65536)",
        )
        .validated(|v| match v {
            Value::Int(n) if *n >= 65536 => Ok(()),
            _ => Err("must be at least 65536".to_owned()),
        }),
        Key::new(
            HTTP_HOST,
            Value::Ip(IpAddr::from([0, 0, 0, 0])),
            "Address the HTTP server binds to (0.0.0.0 or ::)",
        ),
        Key::new(
            HTTP_PORT,
            Value::Port(0),
            "Port the HTTP server binds to (0 picks a free port)",
        ),
        Key::new(
            HTTP_RESOURCE_BASE_URL,
            Value::String(String::new()),
            "Base URL clients use to fetch served resources (empty derives it)",
        )
        .validated(|v| match v {
            Value::String(s) if s.is_empty() || valid_url(s) => Ok(()),
            _ => Err("must be empty or a url".to_owned()),
        }),
        Key::new(
            tuff_web_ui::sql::ALLOW_WRITE_KEY,
            Value::Bool(false),
            "Let holders of tuff.web.sql.write change a SQLite world from the panel's SQL console; chunk, entity and poi rows can only be changed while their chunks are unloaded, and tables the running server keeps in memory stay read-only",
        ),
        Key::new(
            tuff_web_ui::sql::TIMEOUT_KEY,
            Value::Int(tuff_web_ui::sql::DEFAULT_TIMEOUT_SECONDS),
            "Seconds a statement in the panel's SQL console may run before it is stopped (1 to 3600)",
        )
        .validated(|v| match v {
            Value::Int(n) if (1..=3600).contains(n) => Ok(()),
            _ => Err("must be between 1 and 3600".to_owned()),
        }),
        Key::new(
            STORAGE_SQLITE_ENTITY_INDEX,
            Value::Bool(false),
            "Keep the entity_index table of a SQLite world current on every entity save, if the table exists (create it with tuff_entity_index_rebuild()); costs about 0.1 ms per entity chunk write; read at startup",
        ),
        Key::new(
            HTTP_DETECT_PUBLIC_IP,
            Value::Bool(false),
            "Detect the public address for resource urls by asking public STUN servers (Cloudflare, Google) over UDP; this reveals the server address to them and is off by default",
        ),
        Key::new(
            crate::resource_pack::ENABLED_KEY,
            Value::Bool(true),
            "Offer feature-registered resource packs to clients (the custom content pack is always offered and required)",
        ),
        Key::new(
            crate::custom_pack::PROMPT_KEY,
            Value::String(String::new()),
            "Text shown when offering the custom content pack (empty for none)",
        ),
    ]
    .into_iter()
    .chain(crate::world::anti_xray::config_keys())
    .chain(crate::world::hide_details::config_keys())
    .chain(crate::panel_host::config_keys())
    .chain(crate::storage::backup::config_keys())
    .chain(crate::world::pregen::config_keys())
    .chain(crate::world::dimension::config_keys())
    .chain(crate::world::entity_caps::config_keys())
    .chain(crate::world::generator::config_keys())
    .chain(crate::world::live::config_keys())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn core_defaults() {
        let s = ConfigStore::new();
        assert_eq!(s.get_ip(HTTP_HOST), Some(IpAddr::from([0, 0, 0, 0])));
        assert_eq!(s.get_port(HTTP_PORT), Some(0));
        assert_eq!(s.get_bool(HTTP_DETECT_PUBLIC_IP), Some(false));
        assert_eq!(s.get_string(HTTP_RESOURCE_BASE_URL).as_deref(), Some(""));
    }

    #[tuff_core::watchdog]
    #[test]
    fn typed_set_and_parse() {
        let s = ConfigStore::new();
        s.set_text(HTTP_HOST, "::").unwrap();
        assert_eq!(s.get_ip(HTTP_HOST), Some("::".parse().unwrap()));
        s.set_text(HTTP_PORT, "8123").unwrap();
        assert_eq!(s.get_port(HTTP_PORT), Some(8123));
        assert!(s.set_text(HTTP_PORT, "70000").is_err());
        assert!(s.set_text(HTTP_HOST, "nope").is_err());
        assert_eq!(
            s.set(HTTP_PORT, Value::Int(3)),
            Err(ConfigError::WrongKind {
                key: HTTP_PORT.into(),
                expected: Kind::Port
            })
        );
        assert!(matches!(
            s.set_text("tuff.nothing", "1"),
            Err(ConfigError::Unknown(_))
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn validation_and_reset() {
        let s = ConfigStore::new();
        assert!(s.set_text(HTTP_RESOURCE_BASE_URL, "not a url").is_err());
        s.set_text(HTTP_RESOURCE_BASE_URL, "https://example.com/r")
            .unwrap();
        s.reset(HTTP_RESOURCE_BASE_URL).unwrap();
        assert_eq!(s.get_string(HTTP_RESOURCE_BASE_URL).as_deref(), Some(""));
    }

    #[tuff_core::watchdog]
    #[test]
    fn feature_keys() {
        let s = ConfigStore::new();
        let key =
            Key::new("myfeature.some.key", Value::Int(5), "a number").validated(|v| match v {
                Value::Int(i) if (0..=10).contains(i) => Ok(()),
                _ => Err("out of range".into()),
            });
        s.register(key.clone()).unwrap();
        assert_eq!(
            s.register(key),
            Err(ConfigError::Duplicate("myfeature.some.key".into()))
        );
        assert!(matches!(
            s.register(Key::new("bad name", Value::Bool(true), "")),
            Err(ConfigError::BadName(_))
        ));
        assert_eq!(s.get_int("myfeature.some.key"), Some(5));
        s.set("myfeature.some.key", Value::Int(7)).unwrap();
        assert_eq!(
            s.set("myfeature.some.key", Value::Int(11)),
            Err(ConfigError::Invalid("out of range".into()))
        );
        assert_eq!(s.get_int("myfeature.some.key"), Some(7));
        assert_eq!(
            s.definition("myfeature.some.key").unwrap().description,
            "a number"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn saved_vs_runtime_keys() {
        let s = ConfigStore::new();
        s.register(Key::new("p.rt", Value::Bool(false), "").runtime_only())
            .unwrap();
        s.set("p.rt", Value::Bool(true)).unwrap();
        s.set_text(HTTP_PORT, "9").unwrap();
        assert_eq!(s.saved(), vec![(HTTP_PORT.to_owned(), Value::Port(9))]);

        let t = ConfigStore::new();
        t.register(Key::new("p.rt", Value::Bool(false), "").runtime_only())
            .unwrap();
        let errs = t.load_saved([(HTTP_PORT, "9"), ("p.rt", "true"), ("gone.key", "1")]);
        assert_eq!(errs.len(), 1);
        assert_eq!(t.get_port(HTTP_PORT), Some(9));
        assert_eq!(t.get_bool("p.rt"), Some(false));
    }

    #[tuff_core::watchdog]
    #[test]
    fn unregistered_keys_keep_their_saved_text() {
        let s = ConfigStore::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        s.set_sink(Box::new(move |pairs| log.lock().unwrap().push(pairs)));
        s.load_saved([("features.late.speed", "7")]);
        let key = Key::new("features.late.speed", Value::Int(1), "");
        s.register(key.clone()).unwrap();
        assert_eq!(s.get_int("features.late.speed"), Some(7));
        assert!(s.unregister("features.late.speed"));
        assert!(!s.unregister("features.late.speed"));
        assert_eq!(s.get("features.late.speed"), None);
        s.set_text(HTTP_PORT, "5").unwrap();
        let last = seen.lock().unwrap().last().cloned().unwrap();
        assert!(last.contains(&("features.late.speed".into(), "7".into())));
        s.register(key.clone()).unwrap();
        s.reset("features.late.speed").unwrap();
        let last = seen.lock().unwrap().last().cloned().unwrap();
        assert!(!last.iter().any(|(k, _)| k == "features.late.speed"));
        assert!(s.unregister("features.late.speed"));
        s.register(key).unwrap();
        assert_eq!(s.get_int("features.late.speed"), Some(1));

        s.load_saved([("features.late.mode", "fast")]);
        s.register(Key::new("features.late.mode", Value::Int(2), ""))
            .unwrap();
        assert_eq!(s.get_int("features.late.mode"), Some(2));
        assert!(
            !s.saved_text()
                .iter()
                .any(|(k, _)| k == "features.late.mode")
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn sink_sees_saved_changes_in_order_and_runs_do_not_persist() {
        let s = ConfigStore::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        s.set_sink(Box::new(move |pairs| log.lock().unwrap().push(pairs)));
        s.set_text(HTTP_PORT, "5").unwrap();
        s.set_text_for_run(HTTP_PORT, "6").unwrap();
        assert_eq!(s.get_port(HTTP_PORT), Some(6));
        s.set_text(HTTP_HOST, "::").unwrap();
        s.set_text(HTTP_PORT, "0").unwrap();
        let seen = seen.lock().unwrap();
        let ports: Vec<Option<&str>> = seen
            .iter()
            .map(|p| {
                p.iter()
                    .find(|(k, _)| k == HTTP_PORT)
                    .map(|(_, v)| v.as_str())
            })
            .collect();
        assert_eq!(ports, vec![Some("5"), Some("5"), None]);
        s.set_text_for_run(HTTP_PORT, "7").unwrap();
        assert_eq!(
            s.saved_text(),
            vec![(HTTP_HOST.to_owned(), "::".to_owned())]
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn hints_complete_values() {
        let s = ConfigStore::new();
        assert_eq!(
            s.definition(EXECUTE_ALLOW_OUTSIDE).unwrap().suggestions(),
            vec!["true", "false"]
        );
        let key = Key::new("p.mode", Value::String("a".into()), "").hinted(&["a", "b"]);
        assert_eq!(key.suggestions(), vec!["a", "b"]);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn change_notification() {
        let s = ConfigStore::new();
        let mut rx = s.subscribe();
        s.set(HTTP_PORT, Value::Port(1)).unwrap();
        rx.changed().await.unwrap();
        let c = rx.borrow_and_update().clone().unwrap();
        assert_eq!((c.key.as_str(), c.value), (HTTP_PORT, Value::Port(1)));
        s.set(HTTP_PORT, Value::Port(1)).unwrap();
        assert!(!rx.has_changed().unwrap());
        s.set(HTTP_PORT, Value::Port(2)).unwrap();
        assert!(rx.has_changed().unwrap());
    }
}
