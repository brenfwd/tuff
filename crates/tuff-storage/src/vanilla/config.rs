//! The game's own configuration files in the instance directory:
//! `server.properties`, the pretty-printed JSON lists in the game's field
//! order, and `server-icon.png`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};

use crate::admin::PermissionLevel;
use crate::admin::{
    BanEntry, NameAndId, OpEntry, ServerProperties, UserCacheEntry, format_file_date, now,
    parse_file_date, user_cache,
};
use crate::admin::{format_uuid, parse_uuid_text};
use crate::backend::WorldBackend;
use crate::config::{ConfigStore, ICON_FILE, MAX_FILE_LIST, MAX_ICON_BYTES, validate_file_path};
use crate::sqlite::SqliteBackend;

pub const OPS_FILE: &str = "ops.json";
pub const WHITELIST_FILE: &str = "whitelist.json";
pub const BANNED_PLAYERS_FILE: &str = "banned-players.json";
pub const BANNED_IPS_FILE: &str = "banned-ips.json";
pub const PROPERTIES_FILE: &str = "server.properties";
pub const SETTINGS_FILE: &str = "tuff.properties";
pub const GRANTS_DB_FILE: &str = "tuff.db";
pub const USER_CACHE_FILE: &str = "usercache.json";
const UNKNOWN_SOURCE: &str = "(Unknown)";
const EXPIRES_NEVER: &str = "forever";

/// Configuration files in one directory, like a vanilla server's working
/// directory.
#[derive(Clone)]
pub struct VanillaConfig {
    dir: PathBuf,
    grants: Arc<Mutex<Option<Arc<SqliteBackend>>>>,
}

impl std::fmt::Debug for VanillaConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VanillaConfig")
            .field("dir", &self.dir)
            .finish()
    }
}

impl VanillaConfig {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            grants: Arc::default(),
        }
    }

    /// The tuff database beside the vanilla files; it holds plugin grants
    /// only. Reads do not create it, writes do.
    fn grants_store(&self, create: bool) -> io::Result<Option<Arc<dyn ConfigStore>>> {
        let mut slot = self.grants.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            let file = self.dir.join(GRANTS_DB_FILE);
            if !create && !file.is_file() {
                return Ok(None);
            }
            *slot = Some(Arc::new(SqliteBackend::open(file)?));
        }
        Ok(slot.as_ref().and_then(|backend| backend.config()))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn read_array(&self, file: &str) -> io::Result<Vec<Map<String, Value>>> {
        let Some(text) = read_optional(&self.dir.join(file))? else {
            return Ok(Vec::new());
        };
        let value: Value = serde_json::from_str(&text).map_err(io::Error::other)?;
        Ok(value
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(Value::as_object)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    fn write_array(&self, file: &str, entries: Vec<Value>) -> io::Result<()> {
        let text = if entries.is_empty() {
            "[]".to_owned()
        } else {
            serde_json::to_string_pretty(&Value::Array(entries)).map_err(io::Error::other)?
        };
        self.write_atomic(file, text)
    }

    fn write_atomic(&self, file: &str, contents: impl AsRef<[u8]>) -> io::Result<()> {
        let temp = self.dir.join(format!("{file}.tmp"));
        fs::write(&temp, contents)?;
        fs::rename(&temp, self.dir.join(file))
    }
}

fn read_optional(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn name_and_id(object: &Map<String, Value>) -> Option<NameAndId> {
    let uuid = parse_uuid_text(object.get("uuid")?.as_str()?)?;
    let name = object.get("name")?.as_str()?.to_owned();
    Some(NameAndId { uuid, name })
}

fn put_name_and_id(object: &mut Map<String, Value>, user: &NameAndId) {
    object.insert("uuid".into(), format_uuid(user.uuid).into());
    object.insert("name".into(), user.name.clone().into());
}

fn ban_fields<K>(object: &Map<String, Value>, key: K) -> BanEntry<K> {
    let text = |field: &str| object.get(field).and_then(Value::as_str);
    BanEntry {
        key,
        created: text("created")
            .and_then(parse_file_date)
            .unwrap_or_else(now),
        source: text("source").unwrap_or(UNKNOWN_SOURCE).to_owned(),
        expires: text("expires").and_then(parse_file_date),
        reason: text("reason").map(str::to_owned),
    }
}

fn put_ban_fields<K>(object: &mut Map<String, Value>, entry: &BanEntry<K>) {
    object.insert("created".into(), format_file_date(entry.created).into());
    object.insert("source".into(), entry.source.clone().into());
    object.insert(
        "expires".into(),
        entry
            .expires
            .map_or(EXPIRES_NEVER.to_owned(), format_file_date)
            .into(),
    );
    object.insert(
        "reason".into(),
        entry.reason.clone().map_or(Value::Null, Value::String),
    );
}

impl ConfigStore for VanillaConfig {
    fn name(&self) -> &str {
        super::NAME
    }

    fn read_properties(&self) -> io::Result<Option<ServerProperties>> {
        Ok(read_optional(&self.dir.join(PROPERTIES_FILE))?
            .map(|text| ServerProperties::parse(&text)))
    }

    fn write_properties(&self, properties: &ServerProperties) -> io::Result<()> {
        self.write_atomic(PROPERTIES_FILE, properties.to_text())
    }

    fn read_plugin_grants(&self) -> io::Result<Vec<(String, String)>> {
        match self.grants_store(false)? {
            Some(store) => store.read_plugin_grants(),
            None => Ok(Vec::new()),
        }
    }

    fn write_plugin_grant(&self, plugin: &str, body: Option<&str>) -> io::Result<()> {
        match self.grants_store(body.is_some())? {
            Some(store) => store.write_plugin_grant(plugin, body),
            None => Ok(()),
        }
    }

    fn read_settings(&self) -> io::Result<Vec<(String, String)>> {
        Ok(read_optional(&self.dir.join(SETTINGS_FILE))?
            .map(|text| crate::config::parse_settings(&text))
            .unwrap_or_default())
    }

    fn write_settings(&self, settings: &[(String, String)]) -> io::Result<()> {
        let path = self.dir.join(SETTINGS_FILE);
        let existing = read_optional(&path)?.unwrap_or_default();
        if settings.is_empty() && existing.is_empty() {
            return Ok(());
        }
        self.write_atomic(
            SETTINGS_FILE,
            crate::config::merge_settings(&existing, settings),
        )
    }

    fn read_user_cache(&self) -> io::Result<Vec<UserCacheEntry>> {
        Ok(self
            .read_array(USER_CACHE_FILE)?
            .iter()
            .filter_map(|object| {
                Some(UserCacheEntry {
                    user: name_and_id(object)?,
                    expires: parse_file_date(object.get("expiresOn")?.as_str()?)?,
                })
            })
            .take(user_cache::MAX_ENTRIES)
            .collect())
    }

    fn write_user_cache(&self, entries: &[UserCacheEntry]) -> io::Result<()> {
        let entries: Vec<Value> = entries
            .iter()
            .take(user_cache::MAX_ENTRIES)
            .map(|entry| {
                json!({
                    "name": entry.user.name,
                    "uuid": format_uuid(entry.user.uuid),
                    "expiresOn": format_file_date(entry.expires),
                })
            })
            .collect();
        let text = serde_json::to_string(&entries).map_err(io::Error::other)?;
        self.write_atomic(USER_CACHE_FILE, text)
    }

    fn read_ops(&self) -> io::Result<Vec<OpEntry>> {
        Ok(self
            .read_array(OPS_FILE)?
            .into_iter()
            .filter_map(|object| {
                Some(OpEntry {
                    user: name_and_id(&object)?,
                    level: object
                        .get("level")
                        .and_then(Value::as_i64)
                        .map_or(PermissionLevel::All, PermissionLevel::from_id_clamped),
                    bypasses_player_limit: object
                        .get("bypassesPlayerLimit")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                })
            })
            .collect())
    }

    fn write_ops(&self, ops: &[OpEntry]) -> io::Result<()> {
        let entries = ops
            .iter()
            .map(|op| {
                let mut object = Map::new();
                put_name_and_id(&mut object, &op.user);
                object.insert("level".into(), json!(op.level.id()));
                object.insert(
                    "bypassesPlayerLimit".into(),
                    op.bypasses_player_limit.into(),
                );
                Value::Object(object)
            })
            .collect();
        self.write_array(OPS_FILE, entries)
    }

    fn read_whitelist(&self) -> io::Result<Vec<NameAndId>> {
        Ok(self
            .read_array(WHITELIST_FILE)?
            .iter()
            .filter_map(name_and_id)
            .collect())
    }

    fn write_whitelist(&self, users: &[NameAndId]) -> io::Result<()> {
        let entries = users
            .iter()
            .map(|user| {
                let mut object = Map::new();
                put_name_and_id(&mut object, user);
                Value::Object(object)
            })
            .collect();
        self.write_array(WHITELIST_FILE, entries)
    }

    fn read_bans(&self) -> io::Result<Vec<BanEntry<NameAndId>>> {
        Ok(self
            .read_array(BANNED_PLAYERS_FILE)?
            .iter()
            .filter_map(|object| Some(ban_fields(object, name_and_id(object)?)))
            .collect())
    }

    fn write_bans(&self, bans: &[BanEntry<NameAndId>]) -> io::Result<()> {
        let entries = bans
            .iter()
            .map(|ban| {
                let mut object = Map::new();
                put_name_and_id(&mut object, &ban.key);
                put_ban_fields(&mut object, ban);
                Value::Object(object)
            })
            .collect();
        self.write_array(BANNED_PLAYERS_FILE, entries)
    }

    fn read_ip_bans(&self) -> io::Result<Vec<BanEntry<String>>> {
        Ok(self
            .read_array(BANNED_IPS_FILE)?
            .iter()
            .filter_map(|object| {
                let ip = object.get("ip")?.as_str()?;
                Some(ban_fields(object, ip.to_owned()))
            })
            .collect())
    }

    fn write_ip_bans(&self, bans: &[BanEntry<String>]) -> io::Result<()> {
        let entries = bans
            .iter()
            .map(|ban| {
                let mut object = Map::new();
                object.insert("ip".into(), ban.key.clone().into());
                put_ban_fields(&mut object, ban);
                Value::Object(object)
            })
            .collect();
        self.write_array(BANNED_IPS_FILE, entries)
    }

    fn read_icon(&self) -> io::Result<Option<Vec<u8>>> {
        let path = self.dir.join(ICON_FILE);
        let size = match fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        if size > MAX_ICON_BYTES as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is larger than {MAX_ICON_BYTES} bytes", path.display()),
            ));
        }
        fs::read(path).map(Some)
    }

    fn write_icon(&self, png: Option<&[u8]>) -> io::Result<()> {
        let path = self.dir.join(ICON_FILE);
        match png {
            Some(png) => fs::write(path, png),
            None => match fs::remove_file(path) {
                Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
                _ => Ok(()),
            },
        }
    }

    fn list_files(&self, prefix: &str) -> io::Result<Vec<String>> {
        validate_file_path(prefix)?;
        let mut out = Vec::new();
        let mut pending = vec![prefix.to_owned()];
        while let Some(dir) = pending.pop() {
            let entries = match fs::read_dir(self.dir.join(&dir)) {
                Ok(entries) => entries,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err),
            };
            for entry in entries {
                let entry = entry?;
                let Some(name) = entry.file_name().to_str().map(|n| format!("{dir}/{n}")) else {
                    continue;
                };
                if validate_file_path(&name).is_err() {
                    continue;
                }
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    pending.push(name);
                } else if kind.is_file() {
                    if out.len() >= MAX_FILE_LIST {
                        return Err(io::Error::other("too many server files"));
                    }
                    out.push(name);
                }
            }
        }
        out.sort();
        Ok(out)
    }

    fn read_file(&self, path: &str, max: usize) -> io::Result<Option<Vec<u8>>> {
        validate_file_path(path)?;
        let path = self.dir.join(path);
        match fs::symlink_metadata(&path) {
            Ok(meta) if !meta.is_file() => {
                return Err(io::Error::other("server file is not a regular file"));
            }
            Ok(meta) if meta.len() > max as u64 => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("server file is over {max} bytes"),
                ));
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        }
        use std::io::Read;
        let mut out = Vec::new();
        fs::File::open(&path)?
            .take(max as u64 + 1)
            .read_to_end(&mut out)?;
        if out.len() > max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("server file is over {max} bytes"),
            ));
        }
        Ok(Some(out))
    }

    fn write_file(&self, path: &str, contents: &[u8]) -> io::Result<()> {
        validate_file_path(path)?;
        let target = self.dir.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = self.dir.join(format!("{path}.tmp"));
        fs::write(&temp, contents)?;
        fs::rename(&temp, &target)
    }

    fn append_file(&self, path: &str, contents: &[u8]) -> io::Result<()> {
        validate_file_path(path)?;
        let target = self.dir.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        use std::io::Write;
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(target)?
            .write_all(contents)
    }

    fn remove_file(&self, path: &str) -> io::Result<()> {
        validate_file_path(path)?;
        match fs::remove_file(self.dir.join(path)) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::fixtures::{round_trips_every_record, sample_lists, user};
    use crate::test_dir::TestDir;

    fn read_json(dir: &TestDir, file: &str) -> Value {
        serde_json::from_str(&fs::read_to_string(dir.path().join(file)).unwrap()).unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_record_round_trips() {
        let dir = TestDir::new();
        round_trips_every_record(&VanillaConfig::new(dir.path()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_user_cache_reads_and_writes_the_games_file() {
        let dir = TestDir::new();
        let store = VanillaConfig::new(dir.path());
        fs::write(
            dir.path().join(USER_CACHE_FILE),
            r#"[{"name":"Steve","uuid":"8667ba71-b85a-4004-af54-457a9734eed7","expiresOn":"2026-11-03 14:30:00 -0100"},{"name":"bad","uuid":"nope","expiresOn":"2026-11-03 14:30:00 +0000"},{"name":"NoDate","uuid":"8667ba71-b85a-4004-af54-457a9734eed8"}]"#,
        )
        .unwrap();
        let entries = store.read_user_cache().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].user.name, "Steve");
        assert_eq!(entries[0].user.uuid, 0x8667ba71_b85a_4004_af54_457a9734eed7);
        assert_eq!(
            format_file_date(entries[0].expires),
            "2026-11-03 15:30:00 +0000"
        );
        store.write_user_cache(&entries).unwrap();
        let text = fs::read_to_string(dir.path().join(USER_CACHE_FILE)).unwrap();
        assert_eq!(
            text,
            r#"[{"name":"Steve","uuid":"8667ba71-b85a-4004-af54-457a9734eed7","expiresOn":"2026-11-03 15:30:00 +0000"}]"#
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn lists_are_written_in_the_games_format() {
        let dir = TestDir::new();
        let store = VanillaConfig::new(dir.path());
        let lists = sample_lists();
        store.write_ops(&lists.ops).unwrap();
        store.write_whitelist(&lists.whitelist[..1]).unwrap();
        store.write_bans(&lists.bans).unwrap();
        store.write_ip_bans(&lists.ip_bans).unwrap();

        let ops = read_json(&dir, OPS_FILE);
        let keys: Vec<&String> = ops[0].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["uuid", "name", "level", "bypassesPlayerLimit"]);
        assert_eq!(ops[0]["uuid"], format_uuid(user("Admin").uuid));
        assert_eq!(ops[0]["level"], 4);
        let bans = read_json(&dir, BANNED_PLAYERS_FILE);
        let keys: Vec<&String> = bans[0].as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["uuid", "name", "created", "source", "expires", "reason"]
        );
        assert_eq!(bans[0]["expires"], "forever");
        assert_eq!(bans[0]["created"], "2023-11-14 22:13:20 +0000");
        let ips = read_json(&dir, BANNED_IPS_FILE);
        assert_eq!(ips[0]["ip"], "10.0.0.7");
        assert_eq!(ips[0]["expires"], "2023-11-14 23:13:20 +0000");
        assert!(ips[0]["reason"].is_null());
        assert_eq!(
            fs::read_to_string(dir.path().join(WHITELIST_FILE)).unwrap(),
            format!(
                "[\n  {{\n    \"uuid\": \"{}\",\n    \"name\": \"Friend\"\n  }}\n]",
                format_uuid(user("Friend").uuid)
            )
        );
        store.write_whitelist(&[]).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join(WHITELIST_FILE)).unwrap(),
            "[]"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn properties_are_written_as_the_game_writes_them() {
        let dir = TestDir::new();
        let store = VanillaConfig::new(dir.path());
        store
            .write_properties(&ServerProperties::parse("motd=Hi\n"))
            .unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join(PROPERTIES_FILE)).unwrap(),
            "#Minecraft server properties\nenforce-whitelist=false\n\
             function-permission-level=2\nmotd=Hi\nop-permission-level=4\nwhite-list=true\n"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn hand_edited_files_are_read_leniently() {
        let dir = TestDir::new();
        fs::write(
            dir.path().join(OPS_FILE),
            format!(
                r#"[{{"uuid": "{}", "name": "Admin"}}, {{"name": "no uuid"}}, 7]"#,
                format_uuid(user("Admin").uuid)
            ),
        )
        .unwrap();
        fs::write(dir.path().join(BANNED_IPS_FILE), r#"{"not": "a list"}"#).unwrap();
        let store = VanillaConfig::new(dir.path());
        let ops = store.read_ops().unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].level, PermissionLevel::All);
        assert!(!ops[0].bypasses_player_limit);
        assert!(store.read_ip_bans().unwrap().is_empty());
        fs::write(dir.path().join(WHITELIST_FILE), "[").unwrap();
        assert!(store.read_whitelist().is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn out_of_range_op_levels_are_clamped() {
        let dir = TestDir::new();
        fs::write(
            dir.path().join(OPS_FILE),
            format!(
                r#"[{{"uuid": "{}", "name": "Admin", "level": 9}}, {{"uuid": "{}", "name": "Bob", "level": -3}}]"#,
                format_uuid(user("Admin").uuid),
                format_uuid(user("Bob").uuid)
            ),
        )
        .unwrap();
        let ops = VanillaConfig::new(dir.path()).read_ops().unwrap();
        assert_eq!(ops[0].level, PermissionLevel::Owners);
        assert_eq!(ops[1].level, PermissionLevel::All);
    }

    #[tuff_core::watchdog]
    #[test]
    fn oversized_icons_are_refused() {
        let dir = TestDir::new();
        let file = fs::File::create(dir.path().join(ICON_FILE)).unwrap();
        file.set_len(MAX_ICON_BYTES as u64 + 1).unwrap();
        let err = VanillaConfig::new(dir.path()).read_icon().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
