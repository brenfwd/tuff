//! The server configuration as tables of the server file, plus the icon
//! in `sqlar`. List tables keep the list's order in `position`; UUIDs are
//! hyphenated text, dates are seconds since the epoch and a NULL `expires`
//! means the ban never expires.

use std::io;

use rusqlite::{Connection, params};

use super::Db;
use crate::admin::PermissionLevel;
use crate::admin::{BanEntry, NameAndId, OpEntry, ServerProperties, UserCacheEntry, user_cache};
use crate::admin::{format_uuid, parse_uuid_text};
use crate::config::{ConfigStore, ICON_FILE, MAX_FILE_LIST, MAX_ICON_BYTES, validate_file_path};
use crate::sqlar;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS properties(
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS tuff_settings(
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS ops(
    position INTEGER PRIMARY KEY,
    uuid TEXT NOT NULL,
    name TEXT NOT NULL,
    level INTEGER NOT NULL,
    bypasses_player_limit INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS whitelist(
    position INTEGER PRIMARY KEY,
    uuid TEXT NOT NULL,
    name TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS banned_players(
    position INTEGER PRIMARY KEY,
    uuid TEXT NOT NULL,
    name TEXT NOT NULL,
    created INTEGER NOT NULL,
    source TEXT NOT NULL,
    expires INTEGER,
    reason TEXT
);
CREATE TABLE IF NOT EXISTS banned_ips(
    position INTEGER PRIMARY KEY,
    ip TEXT NOT NULL,
    created INTEGER NOT NULL,
    source TEXT NOT NULL,
    expires INTEGER,
    reason TEXT
);
";

fn read_properties(conn: &Connection) -> rusqlite::Result<Option<ServerProperties>> {
    let entries: Vec<(String, String)> = conn
        .prepare_cached("SELECT key, value FROM properties ORDER BY key")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok((!entries.is_empty()).then(|| ServerProperties::from_entries(entries)))
}

fn write_properties(conn: &Connection, properties: &ServerProperties) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM properties", [])?;
    let mut insert = conn.prepare_cached("INSERT INTO properties(key, value) VALUES (?1, ?2)")?;
    for (key, value) in properties.entries() {
        insert.execute(params![key, value])?;
    }
    Ok(())
}

const ENSURE_SETTINGS: &str = "CREATE TABLE IF NOT EXISTS tuff_settings(
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
)";

fn read_settings(conn: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    conn.execute_batch(ENSURE_SETTINGS)?;
    conn.prepare_cached("SELECT key, value FROM tuff_settings ORDER BY key")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

fn write_settings(conn: &Connection, settings: &[(String, String)]) -> rusqlite::Result<()> {
    conn.execute_batch(ENSURE_SETTINGS)?;
    conn.execute("DELETE FROM tuff_settings", [])?;
    let mut insert =
        conn.prepare_cached("INSERT INTO tuff_settings(key, value) VALUES (?1, ?2)")?;
    for (key, value) in settings {
        insert.execute(params![key, value])?;
    }
    Ok(())
}

const ENSURE_USER_CACHE: &str = "CREATE TABLE IF NOT EXISTS user_cache(
    position INTEGER PRIMARY KEY,
    uuid TEXT NOT NULL,
    name TEXT NOT NULL,
    expires INTEGER NOT NULL
)";

fn read_user_cache(conn: &Connection) -> rusqlite::Result<Vec<UserCacheEntry>> {
    conn.execute_batch(ENSURE_USER_CACHE)?;
    let rows = conn
        .prepare_cached("SELECT uuid, name, expires FROM user_cache ORDER BY position LIMIT ?1")?
        .query_map([user_cache::MAX_ENTRIES as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(uuid, name, expires)| {
            Some(UserCacheEntry {
                user: user(&uuid, name)?,
                expires,
            })
        })
        .collect())
}

fn write_user_cache(conn: &Connection, entries: &[UserCacheEntry]) -> rusqlite::Result<()> {
    conn.execute_batch(ENSURE_USER_CACHE)?;
    conn.execute("DELETE FROM user_cache", [])?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO user_cache(position, uuid, name, expires) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (i, entry) in entries.iter().take(user_cache::MAX_ENTRIES).enumerate() {
        insert.execute(params![
            i as i64,
            format_uuid(entry.user.uuid),
            entry.user.name,
            entry.expires
        ])?;
    }
    Ok(())
}

const ENSURE_PLUGIN_GRANTS: &str = "CREATE TABLE IF NOT EXISTS plugin_grants(
    plugin TEXT PRIMARY KEY,
    body TEXT NOT NULL
)";

fn read_plugin_grants(conn: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    conn.execute_batch(ENSURE_PLUGIN_GRANTS)?;
    conn.prepare_cached("SELECT plugin, body FROM plugin_grants ORDER BY plugin")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

fn write_plugin_grant(
    conn: &Connection,
    (plugin, body): &(String, Option<String>),
) -> rusqlite::Result<()> {
    conn.execute_batch(ENSURE_PLUGIN_GRANTS)?;
    match body {
        Some(body) => conn.execute(
            "INSERT OR REPLACE INTO plugin_grants(plugin, body) VALUES (?1, ?2)",
            params![plugin, body],
        )?,
        None => conn.execute(
            "DELETE FROM plugin_grants WHERE plugin = ?1",
            params![plugin],
        )?,
    };
    Ok(())
}

fn user(uuid: &str, name: String) -> Option<NameAndId> {
    Some(NameAndId {
        uuid: parse_uuid_text(uuid)?,
        name,
    })
}

/// Rows whose UUID does not parse are skipped, as the game skips list
/// entries it cannot read.
fn read_ops(conn: &Connection) -> rusqlite::Result<Vec<OpEntry>> {
    let rows = conn
        .prepare_cached(
            "SELECT uuid, name, level, bypasses_player_limit FROM ops ORDER BY position",
        )?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, bool>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(uuid, name, level, bypasses_player_limit)| {
            Some(OpEntry {
                user: user(&uuid, name)?,
                level: PermissionLevel::from_id(level).unwrap_or(PermissionLevel::All),
                bypasses_player_limit,
            })
        })
        .collect())
}

fn write_ops(conn: &Connection, ops: &[OpEntry]) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM ops", [])?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO ops(position, uuid, name, level, bypasses_player_limit) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for (position, op) in ops.iter().enumerate() {
        insert.execute(params![
            position as i64,
            format_uuid(op.user.uuid),
            op.user.name,
            op.level.id(),
            op.bypasses_player_limit
        ])?;
    }
    Ok(())
}

fn read_whitelist(conn: &Connection) -> rusqlite::Result<Vec<NameAndId>> {
    let rows = conn
        .prepare_cached("SELECT uuid, name FROM whitelist ORDER BY position")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(uuid, name)| user(&uuid, name))
        .collect())
}

fn write_whitelist(conn: &Connection, users: &[NameAndId]) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM whitelist", [])?;
    let mut insert =
        conn.prepare_cached("INSERT INTO whitelist(position, uuid, name) VALUES (?1, ?2, ?3)")?;
    for (position, user) in users.iter().enumerate() {
        insert.execute(params![position as i64, format_uuid(user.uuid), user.name])?;
    }
    Ok(())
}

type BanRow = (i64, String, Option<i64>, Option<String>);

fn ban_row(row: &rusqlite::Row<'_>, first: usize) -> rusqlite::Result<BanRow> {
    Ok((
        row.get(first)?,
        row.get(first + 1)?,
        row.get(first + 2)?,
        row.get(first + 3)?,
    ))
}

fn ban<K>(key: K, (created, source, expires, reason): BanRow) -> BanEntry<K> {
    BanEntry {
        key,
        created,
        source,
        expires,
        reason,
    }
}

fn read_bans(conn: &Connection) -> rusqlite::Result<Vec<BanEntry<NameAndId>>> {
    let rows = conn
        .prepare_cached(
            "SELECT uuid, name, created, source, expires, reason FROM banned_players \
             ORDER BY position",
        )?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                ban_row(row, 2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(uuid, name, fields)| Some(ban(user(&uuid, name)?, fields)))
        .collect())
}

fn write_bans(conn: &Connection, bans: &[BanEntry<NameAndId>]) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM banned_players", [])?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO banned_players(position, uuid, name, created, source, expires, reason) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for (position, ban) in bans.iter().enumerate() {
        insert.execute(params![
            position as i64,
            format_uuid(ban.key.uuid),
            ban.key.name,
            ban.created,
            ban.source,
            ban.expires,
            ban.reason
        ])?;
    }
    Ok(())
}

fn read_ip_bans(conn: &Connection) -> rusqlite::Result<Vec<BanEntry<String>>> {
    conn.prepare_cached(
        "SELECT ip, created, source, expires, reason FROM banned_ips ORDER BY position",
    )?
    .query_map([], |row| {
        Ok(ban(row.get::<_, String>(0)?, ban_row(row, 1)?))
    })?
    .collect()
}

fn write_ip_bans(conn: &Connection, bans: &[BanEntry<String>]) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM banned_ips", [])?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO banned_ips(position, ip, created, source, expires, reason) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for (position, ban) in bans.iter().enumerate() {
        insert.execute(params![
            position as i64,
            ban.key,
            ban.created,
            ban.source,
            ban.expires,
            ban.reason
        ])?;
    }
    Ok(())
}

/// The server file's configuration, served by the database thread.
pub struct SqliteConfig {
    pub(super) db: Db,
}

impl SqliteConfig {
    fn read<T: Send + 'static>(
        &self,
        read: fn(&Connection) -> rusqlite::Result<T>,
    ) -> io::Result<T> {
        self.db.run(move |conn| read(conn))
    }

    fn replace<T: Send + 'static>(
        &self,
        records: T,
        write: fn(&Connection, &T) -> rusqlite::Result<()>,
    ) -> io::Result<()> {
        self.db.run(move |conn| {
            let tx = conn.transaction()?;
            write(&tx, &records)?;
            tx.commit()
        })
    }
}

impl ConfigStore for SqliteConfig {
    fn name(&self) -> &str {
        super::NAME
    }

    fn read_properties(&self) -> io::Result<Option<ServerProperties>> {
        self.read(read_properties)
    }

    fn write_properties(&self, properties: &ServerProperties) -> io::Result<()> {
        self.replace(properties.clone(), write_properties)
    }

    fn read_settings(&self) -> io::Result<Vec<(String, String)>> {
        self.read(read_settings)
    }

    fn write_settings(&self, settings: &[(String, String)]) -> io::Result<()> {
        self.replace(settings.to_vec(), |conn, settings| {
            write_settings(conn, settings)
        })
    }

    fn read_user_cache(&self) -> io::Result<Vec<UserCacheEntry>> {
        self.read(read_user_cache)
    }

    fn write_user_cache(&self, entries: &[UserCacheEntry]) -> io::Result<()> {
        self.replace(entries.to_vec(), |conn, entries| {
            write_user_cache(conn, entries)
        })
    }

    fn read_plugin_grants(&self) -> io::Result<Vec<(String, String)>> {
        self.read(read_plugin_grants)
    }

    fn write_plugin_grant(&self, plugin: &str, body: Option<&str>) -> io::Result<()> {
        self.replace(
            (plugin.to_owned(), body.map(str::to_owned)),
            write_plugin_grant,
        )
    }

    fn read_ops(&self) -> io::Result<Vec<OpEntry>> {
        self.read(read_ops)
    }

    fn write_ops(&self, ops: &[OpEntry]) -> io::Result<()> {
        self.replace(ops.to_vec(), |conn, ops| write_ops(conn, ops))
    }

    fn read_whitelist(&self) -> io::Result<Vec<NameAndId>> {
        self.read(read_whitelist)
    }

    fn write_whitelist(&self, users: &[NameAndId]) -> io::Result<()> {
        self.replace(users.to_vec(), |conn, users| write_whitelist(conn, users))
    }

    fn read_bans(&self) -> io::Result<Vec<BanEntry<NameAndId>>> {
        self.read(read_bans)
    }

    fn write_bans(&self, bans: &[BanEntry<NameAndId>]) -> io::Result<()> {
        self.replace(bans.to_vec(), |conn, bans| write_bans(conn, bans))
    }

    fn read_ip_bans(&self) -> io::Result<Vec<BanEntry<String>>> {
        self.read(read_ip_bans)
    }

    fn write_ip_bans(&self, bans: &[BanEntry<String>]) -> io::Result<()> {
        self.replace(bans.to_vec(), |conn, bans| write_ip_bans(conn, bans))
    }

    fn read_icon(&self) -> io::Result<Option<Vec<u8>>> {
        self.db
            .run_io(|conn| sqlar::read(conn, ICON_FILE, MAX_ICON_BYTES))
    }

    fn write_icon(&self, png: Option<&[u8]>) -> io::Result<()> {
        let png = png.map(<[u8]>::to_vec);
        self.db.run(move |conn| match png {
            Some(png) => sqlar::write(conn, ICON_FILE, &png),
            None => sqlar::remove(conn, ICON_FILE),
        })
    }

    fn list_files(&self, prefix: &str) -> io::Result<Vec<String>> {
        validate_file_path(prefix)?;
        let like = format!("{FILE_AREA}{prefix}/%");
        self.db.run_io(move |conn| {
            let names: Vec<String> = conn
                .prepare_cached(
                    "SELECT name FROM sqlar WHERE name LIKE ?1 ESCAPE '\\' AND data IS NOT NULL ORDER BY name LIMIT ?2",
                )
                .and_then(|mut statement| {
                    statement
                        .query_map(
                            rusqlite::params![like.replace('_', "\\_"), MAX_FILE_LIST as i64 + 1],
                            |row| row.get(0),
                        )?
                        .collect()
                })
                .map_err(io::Error::other)?;
            if names.len() > MAX_FILE_LIST {
                return Err(io::Error::other("too many server files"));
            }
            Ok(names
                .into_iter()
                .filter_map(|n| n.strip_prefix(FILE_AREA).map(str::to_owned))
                .collect())
        })
    }

    fn read_file(&self, path: &str, max: usize) -> io::Result<Option<Vec<u8>>> {
        validate_file_path(path)?;
        let name = format!("{FILE_AREA}{path}");
        self.db.run_io(move |conn| sqlar::read(conn, &name, max))
    }

    fn write_file(&self, path: &str, contents: &[u8]) -> io::Result<()> {
        validate_file_path(path)?;
        let name = format!("{FILE_AREA}{path}");
        let contents = contents.to_vec();
        self.db
            .run(move |conn| sqlar::write(conn, &name, &contents))
    }

    fn append_file(&self, path: &str, contents: &[u8]) -> io::Result<()> {
        validate_file_path(path)?;
        let name = format!("{FILE_AREA}{path}");
        let contents = contents.to_vec();
        self.db.run_io(move |conn| {
            let tx = conn.transaction().map_err(io::Error::other)?;
            let mut all = sqlar::read(&tx, &name, MAX_APPEND_FILE)?.unwrap_or_default();
            all.extend_from_slice(&contents);
            if all.len() > MAX_APPEND_FILE {
                return Err(io::Error::other("server file too large to append to"));
            }
            sqlar::write(&tx, &name, &all).map_err(io::Error::other)?;
            tx.commit().map_err(io::Error::other)
        })
    }

    fn remove_file(&self, path: &str) -> io::Result<()> {
        validate_file_path(path)?;
        let name = format!("{FILE_AREA}{path}");
        self.db.run(move |conn| sqlar::remove(conn, &name))
    }
}

/// Where the file area lives among the archive's entries.
const FILE_AREA: &str = "server/";
/// Largest file an append rewrites.
const MAX_APPEND_FILE: usize = 64 << 20;
