//! Server configuration kept outside the world: `server.properties`, the
//! operator, whitelist and ban lists, and the server icon.
//!
//! Like [`WorldBackend`](crate::backend::WorldBackend), a [`ConfigStore`] only
//! decides where the records live. Every method blocks, so callers run
//! them on the blocking pool (the world's config actor does), never on an
//! async task. A world backend that can hold the configuration itself
//! offers a store through [`WorldBackend::config`](crate::backend::WorldBackend::config);
//! otherwise the server falls back to `VanillaConfig`
//! in the instance directory. An embedding program or plugin can supply
//! its own store instead of either.

use std::io;

use crate::admin::{BanEntry, NameAndId, OpEntry, ServerProperties, UserCacheEntry, UserLists};

/// The server icon's name wherever it is stored as a file.
pub const ICON_FILE: &str = "server-icon.png";
/// Largest server icon read back, in bytes.
pub const MAX_ICON_BYTES: usize = 4 << 20;

/// Parses `key=value` lines, skipping blanks and `#` comments.
pub fn parse_settings(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            Some((key.trim().to_owned(), unescape(value)))
        })
        .collect()
}

fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let last = value.chars().count().saturating_sub(1);
    for (i, c) in value.chars().enumerate() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            ' ' if i == 0 || i == last => out.push_str("\\s"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.trim().chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('s') => out.push(' '),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Rewrites `existing` so it holds exactly `settings`: lines for keys
/// still present get the new value in place, lines for dropped keys
/// disappear, comments and blank lines stay, new keys are appended.
pub fn merge_settings(existing: &str, settings: &[(String, String)]) -> String {
    let mut remaining: std::collections::BTreeMap<&str, &str> = settings
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let mut out = String::new();
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let Some((key, _)) = trimmed.split_once('=') else {
            continue;
        };
        if let Some(value) = remaining.remove(key.trim()) {
            out.push_str(&format!("{}={}\n", key.trim(), escape(value)));
        }
    }
    if out.trim().is_empty() && !remaining.is_empty() {
        out = String::from("# tuff settings\n");
    }
    for (key, value) in remaining {
        out.push_str(&format!("{key}={}\n", escape(value)));
    }
    out
}

/// Longest file area path, in bytes.
pub const MAX_FILE_PATH: usize = 200;
/// Most names one file area listing returns.
pub const MAX_FILE_LIST: usize = 10_000;

/// Checks a file area path: `/`-separated segments of ASCII letters,
/// digits, `-`, `_` and `.`, none empty, `.` or `..`, nor starting with
/// a dot.
pub fn validate_file_path(path: &str) -> io::Result<()> {
    let ok = !path.is_empty()
        && path.len() <= MAX_FILE_PATH
        && path.split('/').all(|segment| {
            !segment.is_empty()
                && !segment.starts_with('.')
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        });
    if ok {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid server file path {path:?}"),
        ))
    }
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "this config store has no file area",
    )
}

pub trait ConfigStore: Send + Sync + 'static {
    fn name(&self) -> &str;
    /// `None` when no properties have been stored yet.
    fn read_properties(&self) -> io::Result<Option<ServerProperties>>;
    fn write_properties(&self, properties: &ServerProperties) -> io::Result<()>;
    fn read_ops(&self) -> io::Result<Vec<OpEntry>>;
    fn write_ops(&self, ops: &[OpEntry]) -> io::Result<()>;
    fn read_whitelist(&self) -> io::Result<Vec<NameAndId>>;
    fn write_whitelist(&self, users: &[NameAndId]) -> io::Result<()>;
    fn read_bans(&self) -> io::Result<Vec<BanEntry<NameAndId>>>;
    fn write_bans(&self, bans: &[BanEntry<NameAndId>]) -> io::Result<()>;
    fn read_ip_bans(&self) -> io::Result<Vec<BanEntry<String>>>;
    fn write_ip_bans(&self, bans: &[BanEntry<String>]) -> io::Result<()>;
    /// The server icon as PNG bytes.
    fn read_icon(&self) -> io::Result<Option<Vec<u8>>>;
    /// `None` removes the icon.
    fn write_icon(&self, png: Option<&[u8]>) -> io::Result<()>;

    /// The server's `tuff.*` and plugin settings as key and text pairs,
    /// unknown keys included. Stores that keep none return nothing.
    fn read_settings(&self) -> io::Result<Vec<(String, String)>> {
        Ok(Vec::new())
    }

    /// Replaces every stored setting with `settings`.
    fn write_settings(&self, _settings: &[(String, String)]) -> io::Result<()> {
        Ok(())
    }

    /// Players seen before, most recently used first. Stores that keep
    /// none return nothing.
    fn read_user_cache(&self) -> io::Result<Vec<UserCacheEntry>> {
        Ok(Vec::new())
    }

    /// Replaces the stored user cache.
    fn write_user_cache(&self, _entries: &[UserCacheEntry]) -> io::Result<()> {
        Ok(())
    }

    /// What the owner granted each runtime plugin, as `(plugin id, text
    /// body)` rows. Stores that keep none return nothing.
    fn read_plugin_grants(&self) -> io::Result<Vec<(String, String)>> {
        Ok(Vec::new())
    }

    /// Replaces the stored grant of one plugin; `None` removes it. Stores
    /// that keep no grants refuse this.
    fn write_plugin_grant(&self, _plugin: &str, _body: Option<&str>) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "plugin grants need the sqlite storage backend",
        ))
    }

    /// Server files (audit logs, sessions, uploads) under `prefix`, a
    /// directory path; names are full paths, sorted, at most
    /// [`MAX_FILE_LIST`].
    fn list_files(&self, _prefix: &str) -> io::Result<Vec<String>> {
        Err(unsupported())
    }

    /// A server file, refusing one over `max` bytes; `None` if missing.
    fn read_file(&self, _path: &str, _max: usize) -> io::Result<Option<Vec<u8>>> {
        Err(unsupported())
    }

    /// Replaces a server file as a whole.
    fn write_file(&self, _path: &str, _contents: &[u8]) -> io::Result<()> {
        Err(unsupported())
    }

    /// Appends to a server file, creating it; one call lands whole.
    fn append_file(&self, _path: &str, _contents: &[u8]) -> io::Result<()> {
        Err(unsupported())
    }

    /// Removes a server file; missing is fine.
    fn remove_file(&self, _path: &str) -> io::Result<()> {
        Err(unsupported())
    }

    fn read_lists(&self) -> io::Result<UserLists> {
        Ok(UserLists {
            ops: self.read_ops()?,
            whitelist: self.read_whitelist()?,
            bans: self.read_bans()?,
            ip_bans: self.read_ip_bans()?,
        })
    }

    /// Copies every record from `source`, e.g. when a server moves into a
    /// new store.
    fn import(&self, source: &dyn ConfigStore) -> io::Result<()> {
        if let Some(properties) = source.read_properties()? {
            self.write_properties(&properties)?;
        }
        let lists = source.read_lists()?;
        self.write_ops(&lists.ops)?;
        self.write_whitelist(&lists.whitelist)?;
        self.write_bans(&lists.bans)?;
        self.write_ip_bans(&lists.ip_bans)?;
        self.write_settings(&source.read_settings()?)?;
        self.write_user_cache(&source.read_user_cache()?)?;
        for (plugin, body) in source.read_plugin_grants()? {
            self.write_plugin_grant(&plugin, Some(&body))?;
        }
        if let Some(icon) = source.read_icon()? {
            self.write_icon(Some(&icon))?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for dyn ConfigStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ConfigStore").field(&self.name()).finish()
    }
}

/// Records and a round trip check for testing a [`ConfigStore`].
pub mod fixtures {
    use super::*;
    use crate::admin::PermissionLevel;

    pub fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&width.to_be_bytes());
        png.extend_from_slice(&height.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        png
    }

    pub fn user(name: &str) -> NameAndId {
        NameAndId {
            uuid: name.bytes().fold(0x006f_6666_6c69_6e65_u128, |h, b| {
                h.wrapping_mul(0x0100_0000_01b3).wrapping_add(u128::from(b))
            }),
            name: name.into(),
        }
    }

    /// One of every kind of record, with every optional field both set and
    /// unset somewhere.
    pub fn sample_lists() -> UserLists {
        let mut ban = BanEntry::new(user("Griefer"), "Server".into(), Some("no".into()));
        ban.created = 1_700_000_000;
        let mut timed = BanEntry::new(user("Spammer"), "Admin".into(), None);
        timed.created = 1_600_000_000;
        timed.expires = Some(4_102_444_800);
        let mut ip_ban = BanEntry::new("10.0.0.7".to_owned(), "Admin".into(), None);
        ip_ban.created = 1_700_000_000;
        ip_ban.expires = Some(1_700_003_600);
        let mut v6_ban = BanEntry::new("::1".to_owned(), "Server".into(), Some("loop".into()));
        v6_ban.created = 1_700_000_001;
        UserLists {
            ops: vec![
                OpEntry {
                    user: user("Admin"),
                    level: PermissionLevel::Owners,
                    bypasses_player_limit: true,
                },
                OpEntry {
                    user: user("Helper"),
                    level: PermissionLevel::Moderators,
                    bypasses_player_limit: false,
                },
            ],
            whitelist: vec![user("Friend"), user("Zed"), user("Amy")],
            bans: vec![ban, timed],
            ip_bans: vec![ip_ban, v6_ban],
        }
    }

    /// Settings with unknown plugin keys and values that need escaping.
    pub fn sample_settings() -> Vec<(String, String)> {
        [
            ("myplugin.motto", " leading and trailing "),
            ("tuff.http.port", "8123"),
            ("tuff.http.resourcePathBaseUrl", "https://example.com/a=b"),
            ("zz.multi", "line one\nline two\\ \r end"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .to_vec()
    }

    pub fn sample_properties() -> ServerProperties {
        ServerProperties::parse(
            "motd=A \\u00A7aserver\nwhite-list=false\nop-permission-level=2\nlevel-seed=\n",
        )
    }

    /// Writes and reads back every kind of record through `store`.
    pub fn round_trips_every_record(store: &dyn ConfigStore) {
        assert_eq!(store.read_properties().unwrap(), None);
        assert_eq!(store.read_lists().unwrap(), UserLists::default());
        assert_eq!(store.read_icon().unwrap(), None);

        let properties = sample_properties();
        store.write_properties(&properties).unwrap();
        assert_eq!(store.read_properties().unwrap(), Some(properties));
        store
            .write_properties(&ServerProperties::default())
            .unwrap();
        assert_eq!(
            store.read_properties().unwrap(),
            Some(ServerProperties::default())
        );

        let lists = sample_lists();
        store.write_ops(&lists.ops).unwrap();
        store.write_whitelist(&lists.whitelist).unwrap();
        store.write_bans(&lists.bans).unwrap();
        store.write_ip_bans(&lists.ip_bans).unwrap();
        assert_eq!(store.read_lists().unwrap(), lists);

        store.write_whitelist(&lists.whitelist[1..]).unwrap();
        assert_eq!(store.read_whitelist().unwrap(), lists.whitelist[1..]);
        store.write_ops(&[]).unwrap();
        store.write_bans(&[]).unwrap();
        store.write_ip_bans(&[]).unwrap();
        assert!(store.read_ops().unwrap().is_empty());
        assert!(store.read_bans().unwrap().is_empty());
        assert!(store.read_ip_bans().unwrap().is_empty());

        assert!(store.read_settings().unwrap().is_empty());
        let settings = sample_settings();
        store.write_settings(&settings).unwrap();
        assert_eq!(store.read_settings().unwrap(), settings);
        store.write_settings(&settings[1..2]).unwrap();
        assert_eq!(store.read_settings().unwrap(), settings[1..2]);
        store.write_settings(&[]).unwrap();
        assert!(store.read_settings().unwrap().is_empty());

        assert!(store.read_user_cache().unwrap().is_empty());
        let cache: Vec<UserCacheEntry> = sample_lists()
            .whitelist
            .into_iter()
            .enumerate()
            .map(|(i, user)| UserCacheEntry {
                user,
                expires: 1_800_000_000 - i as i64 * 86_400,
            })
            .collect();
        store.write_user_cache(&cache).unwrap();
        assert_eq!(store.read_user_cache().unwrap(), cache);
        store.write_user_cache(&cache[1..]).unwrap();
        assert_eq!(store.read_user_cache().unwrap(), cache[1..]);

        let icon = png_header(64, 64);
        store.write_icon(Some(&icon)).unwrap();
        assert_eq!(store.read_icon().unwrap(), Some(icon));
        let noise: Vec<u8> = (0..4096u32).map(|i| (i * 7919 % 251) as u8).collect();
        store.write_icon(Some(&noise)).unwrap();
        assert_eq!(store.read_icon().unwrap(), Some(noise));
        store.write_icon(None).unwrap();
        assert_eq!(store.read_icon().unwrap(), None);
        store.write_icon(None).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::sample_settings;
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn merging_settings_keeps_comments_and_replaces_in_place() {
        let existing = "# my notes\nb.key=old\n\n# about a\na.key=1\ngone.key=x\n";
        let merged = merge_settings(
            existing,
            &[
                ("a.key".into(), "2".into()),
                ("b.key".into(), "new".into()),
                ("c.key".into(), "3".into()),
            ],
        );
        assert_eq!(
            merged,
            "# my notes\nb.key=new\n\n# about a\na.key=2\nc.key=3\n"
        );
        assert_eq!(
            parse_settings(&merged),
            vec![
                ("b.key".to_owned(), "new".to_owned()),
                ("a.key".to_owned(), "2".to_owned()),
                ("c.key".to_owned(), "3".to_owned()),
            ]
        );
        assert_eq!(
            merge_settings("", &[("k".into(), "v".into())]),
            "# tuff settings\nk=v\n"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn settings_values_survive_escaping() {
        let settings = sample_settings();
        let text = merge_settings("", &settings);
        assert_eq!(parse_settings(&text), settings);
    }
}
