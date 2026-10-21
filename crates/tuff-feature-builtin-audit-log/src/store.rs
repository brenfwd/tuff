//! The SQLite file: schema, batched inserts, filtered reads, bounded
//! purges and rollback marks. Only the writer and reader threads call it.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params, params_from_iter};
use tuff_nbt::{Compound, NamedTag, NbtLimits, Tag};
use tuff_storage::migrate::{self, Step};

use crate::filter::{Area, Filter};
use crate::record::{Kind, Record, Who};

/// `TALG`.
const APPLICATION_ID: i64 = 0x5441_4C47;
const VERSION: i64 = 1;
/// Upgrades from each older format, in order. None yet.
const MIGRATIONS: &[Step<Connection>] = &[];

/// Stored NBT larger than this is dropped when written and refused when
/// read.
pub const MAX_DATA_BYTES: usize = 1 << 20;
/// The largest item a container row keeps whole; a bigger one is logged
/// by id alone.
pub const MAX_ITEM_DATA_BYTES: usize = 64 << 10;
/// Stored NBT is untrusted: depth and size are bounded when read.
const DATA_LIMITS: NbtLimits = NbtLimits {
    max_depth: 64,
    max_bytes: MAX_DATA_BYTES,
};
/// Rows a lookup may return at once.
pub const MAX_ROWS: usize = 100_000;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS users(
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    uuid BLOB
);
CREATE UNIQUE INDEX IF NOT EXISTS users_uuid ON users(uuid) WHERE uuid IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS users_cause ON users(name) WHERE uuid IS NULL;
CREATE INDEX IF NOT EXISTS users_name ON users(name COLLATE NOCASE);
CREATE TABLE IF NOT EXISTS worlds(id INTEGER PRIMARY KEY, key TEXT NOT NULL UNIQUE);
CREATE TABLE IF NOT EXISTS materials(id INTEGER PRIMARY KEY, key TEXT NOT NULL UNIQUE);
CREATE TABLE IF NOT EXISTS states(id INTEGER PRIMARY KEY, state TEXT NOT NULL UNIQUE);
CREATE TABLE IF NOT EXISTS actions(
    id INTEGER PRIMARY KEY,
    time INTEGER NOT NULL,
    user INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    world INTEGER NOT NULL,
    x INTEGER NOT NULL,
    y INTEGER NOT NULL,
    z INTEGER NOT NULL,
    material INTEGER,
    old_state INTEGER,
    new_state INTEGER,
    amount INTEGER NOT NULL DEFAULT 1,
    text TEXT,
    data BLOB,
    rolled_back INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS actions_time ON actions(time);
CREATE INDEX IF NOT EXISTS actions_user ON actions(user, time);
CREATE INDEX IF NOT EXISTS actions_place ON actions(world, x, z, y);
CREATE INDEX IF NOT EXISTS actions_kind ON actions(kind, time);
";

/// Which action kinds a purge may delete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KindScope {
    Any,
    Only(Vec<Kind>),
    Except(Vec<Kind>),
}

impl KindScope {
    fn clause(&self) -> String {
        let codes = |kinds: &[Kind]| {
            kinds
                .iter()
                .map(|kind| kind.code().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        match self {
            KindScope::Any => String::new(),
            KindScope::Only(kinds) if kinds.is_empty() => " AND 0".to_owned(),
            KindScope::Only(kinds) => format!(" AND kind IN ({})", codes(kinds)),
            KindScope::Except(kinds) if kinds.is_empty() => String::new(),
            KindScope::Except(kinds) => format!(" AND kind NOT IN ({})", codes(kinds)),
        }
    }
}

/// A logged row as read back. Strings and data come from the file and are
/// untrusted.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: i64,
    pub time: i64,
    pub user: String,
    /// The player's uuid, `None` for a cause.
    pub uuid: Option<u128>,
    /// `None` for a code this version does not know.
    pub kind: Option<Kind>,
    pub world: String,
    pub pos: [i32; 3],
    pub material: Option<String>,
    pub old: Option<String>,
    pub new: Option<String>,
    pub amount: i64,
    pub text: Option<String>,
    pub data: Option<Vec<u8>>,
    pub rolled_back: bool,
}

impl Row {
    /// The stored block entity, if it decodes within the limits.
    pub fn block_entity(&self) -> Option<Compound> {
        decode_data(self.data.as_deref()?)
    }
}

pub fn encode_data(data: &Compound) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    NamedTag::new("", Tag::Compound(data.clone()))
        .write(&mut out)
        .ok()?;
    (out.len() <= MAX_DATA_BYTES).then_some(out)
}

pub fn decode_data(bytes: &[u8]) -> Option<Compound> {
    if bytes.len() > MAX_DATA_BYTES {
        return None;
    }
    let mut input = bytes;
    match NamedTag::read(&mut input, DATA_LIMITS).ok()?.tag {
        Tag::Compound(compound) => Some(compound),
        _ => None,
    }
}

fn configure(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

/// The writer's connection with its intern caches.
pub struct Store {
    conn: Connection,
    /// Each user's row and, for players, the name it was last stored with.
    users: HashMap<UserKey, (i64, Option<Arc<str>>)>,
    worlds: HashMap<String, i64>,
    materials: HashMap<String, i64>,
    states: HashMap<String, i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum UserKey {
    Player(u128),
    Cause(String),
}

impl Store {
    /// Opens (creating) the file and its schema. A file some other program
    /// made is refused rather than changed.
    pub fn open(path: &Path) -> rusqlite::Result<Store> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let application: i64 = conn.pragma_query_value(None, "application_id", |r| r.get(0))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let tables: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
            [],
            |r| r.get(0),
        )?;
        if (application != APPLICATION_ID && tables > 0) || version > VERSION {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_NOTADB),
                Some(format!(
                    "{} is not an audit log this server can use",
                    path.display()
                )),
            ));
        }
        let mut conn = conn;
        if tables > 0 {
            migrate::run(
                "audit log",
                u32::try_from(version).unwrap_or(0),
                VERSION as u32,
                MIGRATIONS,
                &mut conn,
            )
            .map_err(|err| {
                rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_NOTADB),
                    Some(format!("{}: {err}", path.display())),
                )
            })?;
        }
        configure(&conn)?;
        conn.execute_batch(SCHEMA)?;
        conn.pragma_update(None, "application_id", APPLICATION_ID)?;
        conn.pragma_update(None, "user_version", VERSION)?;
        Ok(Store {
            conn,
            users: HashMap::new(),
            worlds: HashMap::new(),
            materials: HashMap::new(),
            states: HashMap::new(),
        })
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Writes `records` in one transaction; `world` names each record's
    /// dimension.
    pub fn insert<'a>(
        &mut self,
        records: impl IntoIterator<Item = (&'a Record, &'a str)>,
    ) -> rusqlite::Result<usize> {
        let tx = self.conn.transaction()?;
        let mut written = 0;
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO actions(time, user, kind, world, x, y, z, material, old_state,
                 new_state, amount, text, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                 ?11, ?12, ?13)",
            )?;
            for (record, world) in records {
                let mut renamed = None;
                if record.kind == Kind::Username {
                    match renamed_from(&tx, &record.who)? {
                        Some(text) => renamed = Some(text),
                        None => continue,
                    }
                }
                let user = intern_user(&tx, &mut self.users, &record.who)?;
                let world = intern(&tx, &mut self.worlds, "worlds", "key", world)?;
                let material = match record.material_key() {
                    Some(key) => Some(intern(&tx, &mut self.materials, "materials", "key", &key)?),
                    None => None,
                };
                let mut state = |state: Option<tuff_vanilla_data::BlockState>| match state {
                    Some(state) => {
                        intern(&tx, &mut self.states, "states", "state", &state.to_string())
                            .map(Some)
                    }
                    None => Ok(None),
                };
                let old = state(record.old)?;
                let new = state(record.new)?;
                let data = record.data.as_ref().and_then(encode_data).filter(|bytes| {
                    record.kind.is_block()
                        || record.kind == Kind::Sign
                        || bytes.len() <= MAX_ITEM_DATA_BYTES
                });
                insert.execute(params![
                    record.time,
                    user,
                    record.kind.code(),
                    world,
                    record.pos[0],
                    record.pos[1],
                    record.pos[2],
                    material,
                    old,
                    new,
                    record.amount,
                    renamed.as_deref().or(record.text.as_deref()),
                    data,
                ])?;
                written += 1;
            }
        }
        tx.commit()?;
        Ok(written)
    }

    /// Deletes up to `limit` rows older than `before` (in `world` when
    /// given, of the `kinds` the scope names); returns how many went.
    pub fn purge(
        &mut self,
        before: i64,
        world: Option<&str>,
        kinds: &KindScope,
        limit: usize,
    ) -> rusqlite::Result<usize> {
        let mut sql =
            "DELETE FROM actions WHERE id IN (SELECT id FROM actions WHERE time < ?".to_owned();
        let mut values = vec![Value::Integer(before)];
        if let Some(world) = world {
            sql.push_str(" AND world = (SELECT id FROM worlds WHERE key = ?)");
            values.push(Value::Text(world.to_owned()));
        }
        sql.push_str(&kinds.clause());
        sql.push_str(" LIMIT ?)");
        values.push(Value::Integer(limit as i64));
        self.conn.execute(&sql, rusqlite::params_from_iter(values))
    }

    /// Marks rows rolled back (or not, after a restore).
    pub fn mark(&mut self, ids: &[i64], rolled_back: bool) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut update =
                tx.prepare_cached("UPDATE actions SET rolled_back = ?1 WHERE id = ?2")?;
            for id in ids {
                update.execute(params![rolled_back as i64, id])?;
            }
        }
        tx.commit()
    }

    /// Checkpoints the WAL into the file so nothing is left beside it.
    pub fn close(self) {
        let _ = self.conn.pragma_update(None, "wal_checkpoint", "TRUNCATE");
    }
}

fn intern(
    tx: &Transaction<'_>,
    cache: &mut HashMap<String, i64>,
    table: &str,
    column: &str,
    key: &str,
) -> rusqlite::Result<i64> {
    if let Some(id) = cache.get(key) {
        return Ok(*id);
    }
    let found: Option<i64> = tx
        .prepare_cached(&format!("SELECT id FROM {table} WHERE {column} = ?1"))?
        .query_row([key], |r| r.get(0))
        .optional()?;
    let id = match found {
        Some(id) => id,
        None => {
            tx.prepare_cached(&format!("INSERT INTO {table}({column}) VALUES (?1)"))?
                .execute([key])?;
            tx.last_insert_rowid()
        }
    };
    cache.insert(key.to_owned(), id);
    Ok(id)
}

/// `old -> new` when the player's stored name differs from this record's.
fn renamed_from(tx: &Transaction<'_>, who: &Who) -> rusqlite::Result<Option<String>> {
    let Who::Player { uuid, name } = who else {
        return Ok(None);
    };
    let stored: Option<String> = tx
        .prepare_cached("SELECT name FROM users WHERE uuid = ?1")?
        .query_row([uuid.to_be_bytes().to_vec()], |r| r.get(0))
        .optional()?;
    Ok(stored
        .filter(|old| old.as_str() != &**name)
        .map(|old| format!("{old} -> {name}")))
}

fn intern_user(
    tx: &Transaction<'_>,
    cache: &mut HashMap<UserKey, (i64, Option<Arc<str>>)>,
    who: &Who,
) -> rusqlite::Result<i64> {
    let key = match who {
        Who::Player { uuid, .. } => UserKey::Player(*uuid),
        Who::Cause(name) => UserKey::Cause(name.clone()),
    };
    if let Some((id, known)) = cache.get_mut(&key) {
        if let Who::Player { name, .. } = who
            && known.as_ref() != Some(name)
        {
            tx.prepare_cached("UPDATE users SET name = ?1 WHERE id = ?2")?
                .execute(params![&**name, *id])?;
            *known = Some(name.clone());
        }
        return Ok(*id);
    }
    let id = match who {
        Who::Player { uuid, name } => {
            let uuid = uuid.to_be_bytes().to_vec();
            tx.prepare_cached(
                "INSERT INTO users(name, uuid) VALUES (?1, ?2)
                 ON CONFLICT(uuid) WHERE uuid IS NOT NULL DO UPDATE SET name = excluded.name",
            )?
            .execute(params![&**name, uuid])?;
            tx.prepare_cached("SELECT id FROM users WHERE uuid = ?1")?
                .query_row([uuid], |r| r.get(0))?
        }
        Who::Cause(name) => {
            tx.prepare_cached(
                "INSERT INTO users(name, uuid) VALUES (?1, NULL)
                 ON CONFLICT(name) WHERE uuid IS NULL DO NOTHING",
            )?
            .execute([name])?;
            tx.prepare_cached("SELECT id FROM users WHERE name = ?1 AND uuid IS NULL")?
                .query_row([name], |r| r.get(0))?
        }
    };
    let name = match who {
        Who::Player { name, .. } => Some(name.clone()),
        Who::Cause(_) => None,
    };
    cache.insert(key, (id, name));
    Ok(id)
}

/// A read-only connection for lookups beside the writer.
pub fn open_reader(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
}

/// Newest first, then by id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    NewestFirst,
    OldestFirst,
}

/// The WHERE clause and its values for `filter`.
fn conditions(filter: &Filter) -> (String, Vec<Value>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut values: Vec<Value> = Vec::new();
    let placeholders = |values: &mut Vec<Value>, items: &[String]| -> String {
        let marks: Vec<&str> = items.iter().map(|_| "?").collect();
        values.extend(items.iter().map(|item| Value::Text(item.clone())));
        marks.join(", ")
    };
    if !filter.users.is_empty() {
        let marks = placeholders(&mut values, &filter.users);
        clauses.push(format!(
            "a.user IN (SELECT id FROM users WHERE name COLLATE NOCASE IN ({marks}))"
        ));
    }
    if let Some(since) = filter.since {
        clauses.push("a.time >= ?".to_owned());
        values.push(Value::Integer(since));
    }
    if let Some(until) = filter.until {
        clauses.push("a.time <= ?".to_owned());
        values.push(Value::Integer(until));
    }
    match &filter.area {
        Area::Global => {}
        Area::World(world) => {
            clauses.push("a.world = (SELECT id FROM worlds WHERE key = ?)".to_owned());
            values.push(Value::Text(world.clone()));
        }
        Area::Radius {
            world,
            center,
            radius,
        } => {
            clauses.push(
                "a.world = (SELECT id FROM worlds WHERE key = ?) AND a.x BETWEEN ? AND ? \
                 AND a.z BETWEEN ? AND ?"
                    .to_owned(),
            );
            let radius = i64::from(*radius);
            values.push(Value::Text(world.clone()));
            values.push(Value::Integer(i64::from(center[0]) - radius));
            values.push(Value::Integer(i64::from(center[0]) + radius));
            values.push(Value::Integer(i64::from(center[2]) - radius));
            values.push(Value::Integer(i64::from(center[2]) + radius));
        }
    }
    if !filter.kinds.is_empty() {
        let codes: Vec<String> = filter.kinds.iter().map(|k| k.code().to_string()).collect();
        clauses.push(format!("a.kind IN ({})", codes.join(", ")));
    }
    if !filter.include.is_empty() {
        let marks = placeholders(&mut values, &filter.include);
        clauses.push(format!(
            "a.material IN (SELECT id FROM materials WHERE key IN ({marks}))"
        ));
    }
    if !filter.exclude.is_empty() {
        let materials: Vec<String> = filter
            .exclude
            .iter()
            .map(|e| crate::filter::qualified(e))
            .collect();
        let marks = placeholders(&mut values, &materials);
        clauses.push(format!(
            "(a.material IS NULL OR a.material NOT IN (SELECT id FROM materials WHERE key IN ({marks})))"
        ));
        let marks = placeholders(&mut values, &filter.exclude);
        clauses.push(format!(
            "a.user NOT IN (SELECT id FROM users WHERE name COLLATE NOCASE IN ({marks}))"
        ));
    }
    if !filter.row_ids.is_empty() {
        let ids: Vec<String> = filter.row_ids.iter().map(i64::to_string).collect();
        clauses.push(format!("a.id IN ({})", ids.join(", ")));
    }
    if let Some(rolled_back) = filter.rolled_back {
        clauses.push("a.rolled_back = ?".to_owned());
        values.push(Value::Integer(rolled_back as i64));
    }
    let clause = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    (clause, values)
}

/// Rows matching `filter`, `limit` from `offset`.
pub fn lookup(
    conn: &Connection,
    filter: &Filter,
    order: Order,
    limit: usize,
    offset: usize,
) -> rusqlite::Result<Vec<Row>> {
    let (clause, mut values) = conditions(filter);
    let direction = match order {
        Order::NewestFirst => "DESC",
        Order::OldestFirst => "ASC",
    };
    let sql = format!(
        "SELECT a.id, a.time, u.name, a.kind, w.key, a.x, a.y, a.z, m.key, so.state, sn.state,
         a.amount, a.text, a.data, a.rolled_back, u.uuid
         FROM actions a JOIN users u ON u.id = a.user JOIN worlds w ON w.id = a.world
         LEFT JOIN materials m ON m.id = a.material
         LEFT JOIN states so ON so.id = a.old_state
         LEFT JOIN states sn ON sn.id = a.new_state
         {clause} ORDER BY a.time {direction}, a.id {direction} LIMIT ? OFFSET ?"
    );
    values.push(Value::Integer(limit.min(MAX_ROWS) as i64));
    values.push(Value::Integer(offset as i64));
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(values), |r| {
        Ok(Row {
            id: r.get(0)?,
            time: r.get(1)?,
            user: r.get(2)?,
            kind: Kind::from_code(r.get(3)?),
            world: r.get(4)?,
            pos: [r.get(5)?, r.get(6)?, r.get(7)?],
            material: r.get(8)?,
            old: r.get(9)?,
            new: r.get(10)?,
            amount: r.get(11)?,
            text: r.get(12)?,
            data: r.get(13)?,
            rolled_back: r.get::<_, i64>(14)? != 0,
            uuid: r
                .get::<_, Option<Vec<u8>>>(15)?
                .and_then(|bytes| Some(u128::from_be_bytes(bytes.try_into().ok()?))),
        })
    })?;
    rows.collect()
}

/// How many rows match `filter`.
pub fn count(conn: &Connection, filter: &Filter) -> rusqlite::Result<u64> {
    let (clause, values) = conditions(filter);
    let sql = format!("SELECT count(*) FROM actions a {clause}");
    conn.query_row(&sql, params_from_iter(values), |r| r.get::<_, i64>(0))
        .map(|n| n as u64)
}

/// Every known player name, for tab completion.
pub fn user_names(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT name FROM users WHERE uuid IS NOT NULL ORDER BY name LIMIT 5000")?;
    stmt.query_map([], |r| r.get::<_, String>(0))?.collect()
}

/// Every row's count.
pub fn total(conn: &Connection) -> rusqlite::Result<u64> {
    conn.query_row("SELECT count(*) FROM actions", [], |r| r.get::<_, i64>(0))
        .map(|n| n as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_core::world::dimension::DimensionId;
    use tuff_storage::test_dir::TestDir;

    #[tuff_core::watchdog]
    #[test]
    fn older_or_newer_versions_are_refused() {
        assert!(MIGRATIONS.is_empty());
        let dir = TestDir::new();
        let path = dir.path().join("audit.sqlite");
        drop(Store::open(&path).unwrap());
        drop(Store::open(&path).unwrap());
        for version in [VERSION - 1, VERSION + 1] {
            Connection::open(&path)
                .unwrap()
                .pragma_update(None, "user_version", version)
                .unwrap();
            assert!(Store::open(&path).is_err(), "{version}");
        }
    }
    use tuff_vanilla_data::BlockState;

    fn player(name: &str, uuid: u128) -> Who {
        Who::Player {
            uuid,
            name: Arc::from(name),
        }
    }

    fn block(time: i64, who: Who, kind: Kind, pos: [i32; 3], old: &str, new: &str) -> Record {
        let mut record = Record::new(time, who, kind, DimensionId::default(), pos);
        record.old = Some(old.parse::<BlockState>().unwrap());
        record.new = Some(new.parse::<BlockState>().unwrap());
        record
    }

    fn sample(store: &mut Store) {
        let alex = player("Alex", 1);
        let steve = player("Steve", 2);
        let records = [
            block(
                1_000,
                alex.clone(),
                Kind::BlockBreak,
                [0, 64, 0],
                "minecraft:stone",
                "minecraft:air",
            ),
            block(
                2_000,
                alex.clone(),
                Kind::BlockPlace,
                [5, 64, 5],
                "minecraft:air",
                "minecraft:dirt",
            ),
            block(
                3_000,
                steve.clone(),
                Kind::BlockBreak,
                [100, 64, 0],
                "minecraft:oak_log[axis=x]",
                "minecraft:air",
            ),
            block(
                4_000,
                Who::cause("tnt"),
                Kind::BlockBreak,
                [1, 60, 1],
                "minecraft:stone",
                "minecraft:air",
            ),
            {
                let mut chat =
                    Record::new(5_000, steve, Kind::Chat, DimensionId::default(), [0, 0, 0]);
                chat.text = Some("hello".to_owned());
                chat
            },
        ];
        let mut rows: Vec<(&Record, &str)> = records
            .iter()
            .map(|record| (record, "minecraft:overworld"))
            .collect();
        rows[3].1 = "minecraft:the_nether";
        store.insert(rows).unwrap();
    }

    fn ids(conn: &Connection, filter: &Filter) -> Vec<i64> {
        lookup(conn, filter, Order::OldestFirst, 100, 0)
            .unwrap()
            .into_iter()
            .map(|row| row.time / 1000)
            .collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn rows_round_trip_with_their_names() {
        let dir = TestDir::new();
        let mut store = Store::open(&dir.path().join("audit.sqlite")).unwrap();
        sample(&mut store);
        let rows = lookup(
            store.connection(),
            &Filter::default(),
            Order::NewestFirst,
            10,
            0,
        )
        .unwrap();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].text.as_deref(), Some("hello"));
        assert_eq!(rows[0].kind, Some(Kind::Chat));
        assert_eq!(rows[1].user, "#tnt");
        assert_eq!(rows[1].world, "minecraft:the_nether");
        assert_eq!(rows[2].material.as_deref(), Some("minecraft:oak_log"));
        assert_eq!(rows[2].old.as_deref(), Some("minecraft:oak_log[axis=x]"));
        assert_eq!(rows[3].material.as_deref(), Some("minecraft:dirt"));
        assert_eq!(rows[4].pos, [0, 64, 0]);
        assert_eq!(total(store.connection()).unwrap(), 5);
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_filter_narrows_the_rows() {
        let dir = TestDir::new();
        let mut store = Store::open(&dir.path().join("audit.sqlite")).unwrap();
        sample(&mut store);
        let conn = store.connection();
        let with = |f: fn(&mut Filter)| {
            let mut filter = Filter::default();
            f(&mut filter);
            filter
        };
        assert_eq!(ids(conn, &with(|f| f.users = vec!["alex".into()])), [1, 2]);
        assert_eq!(ids(conn, &with(|f| f.users = vec!["#tnt".into()])), [4]);
        assert_eq!(ids(conn, &with(|f| f.since = Some(3_000))), [3, 4, 5]);
        assert_eq!(ids(conn, &with(|f| f.until = Some(2_000))), [1, 2]);
        assert_eq!(
            ids(
                conn,
                &with(|f| f.area = Area::World("minecraft:the_nether".into()))
            ),
            [4]
        );
        assert_eq!(
            ids(
                conn,
                &with(|f| f.area = Area::Radius {
                    world: "minecraft:overworld".into(),
                    center: [2, 0, 2],
                    radius: 3,
                })
            ),
            [1, 2, 5]
        );
        assert_eq!(ids(conn, &with(|f| f.kinds = vec![Kind::BlockPlace])), [2]);
        assert_eq!(
            ids(conn, &with(|f| f.include = vec!["minecraft:stone".into()])),
            [1, 4]
        );
        assert_eq!(
            ids(conn, &with(|f| f.exclude = vec!["stone".into()])),
            [2, 3, 5]
        );
        assert_eq!(
            ids(conn, &with(|f| f.exclude = vec!["Steve".into()])),
            [1, 2, 4]
        );
        assert_eq!(
            count(conn, &with(|f| f.kinds = vec![Kind::BlockBreak])).unwrap(),
            3
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn marks_and_bounded_purges() {
        let dir = TestDir::new();
        let mut store = Store::open(&dir.path().join("audit.sqlite")).unwrap();
        sample(&mut store);
        store.mark(&[1, 2], true).unwrap();
        let rolled = Filter {
            rolled_back: Some(true),
            ..Filter::default()
        };
        assert_eq!(ids(store.connection(), &rolled), [1, 2]);
        store.mark(&[1], false).unwrap();
        assert_eq!(ids(store.connection(), &rolled), [2]);

        assert_eq!(
            store
                .purge(10_000, Some("minecraft:the_nether"), &KindScope::Any, 10)
                .unwrap(),
            1
        );
        assert_eq!(store.purge(4_500, None, &KindScope::Any, 2).unwrap(), 2);
        assert_eq!(store.purge(4_500, None, &KindScope::Any, 2).unwrap(), 1);
        assert_eq!(store.purge(4_500, None, &KindScope::Any, 2).unwrap(), 0);
        assert_eq!(ids(store.connection(), &Filter::default()), [5]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn purges_can_be_limited_to_or_spared_by_kind() {
        let dir = TestDir::new();
        let mut store = Store::open(&dir.path().join("audit.sqlite")).unwrap();
        sample(&mut store);
        let kinds: Vec<_> = Kind::ALL
            .into_iter()
            .filter(|k| *k != Kind::BlockBreak)
            .collect();
        let before = ids(store.connection(), &Filter::default()).len();
        assert_eq!(
            store
                .purge(i64::MAX, None, &KindScope::Only(Vec::new()), 100)
                .unwrap(),
            0
        );
        let broken = store
            .purge(
                i64::MAX,
                None,
                &KindScope::Only(vec![Kind::BlockBreak]),
                100,
            )
            .unwrap();
        assert!(broken > 0 && broken < before);
        let left = ids(store.connection(), &Filter::default()).len();
        assert_eq!(left, before - broken);
        assert_eq!(
            store
                .purge(i64::MAX, None, &KindScope::Except(kinds), 100)
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .purge(i64::MAX, None, &KindScope::Except(Vec::new()), 100)
                .unwrap(),
            left
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_renamed_player_keeps_one_user_row() {
        let dir = TestDir::new();
        let mut store = Store::open(&dir.path().join("audit.sqlite")).unwrap();
        let first = Record::new(
            1,
            player("Old", 7),
            Kind::Chat,
            DimensionId::default(),
            [0; 3],
        );
        store.insert([(&first, "w")]).unwrap();
        drop(store);
        let mut store = Store::open(&dir.path().join("audit.sqlite")).unwrap();
        let second = Record::new(
            2,
            player("New", 7),
            Kind::Chat,
            DimensionId::default(),
            [0; 3],
        );
        store.insert([(&second, "w")]).unwrap();
        let rows = lookup(
            store.connection(),
            &Filter::default(),
            Order::OldestFirst,
            10,
            0,
        )
        .unwrap();
        assert_eq!(
            rows.iter().map(|r| r.user.as_str()).collect::<Vec<_>>(),
            ["New", "New"]
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn block_entities_round_trip_and_bad_data_is_refused() {
        let mut chest = Compound::new();
        chest.insert("id", Tag::String("minecraft:chest".into()));
        let bytes = encode_data(&chest).unwrap();
        assert_eq!(decode_data(&bytes), Some(chest));
        assert_eq!(decode_data(&[]), None);
        assert_eq!(decode_data(&[10, 0, 0, 99, 1, 2, 3]), None);
        assert_eq!(decode_data(&bytes[..bytes.len() - 1]), None);
        assert_eq!(decode_data(&vec![0u8; MAX_DATA_BYTES + 1]), None);
        let mut deep = Tag::Compound(Compound::new());
        for _ in 0..200 {
            deep = Tag::List(vec![deep]);
        }
        let mut nested = Compound::new();
        nested.insert("x", deep);
        let mut out = Vec::new();
        NamedTag::new("", Tag::Compound(nested))
            .write(&mut out)
            .unwrap();
        assert_eq!(decode_data(&out), None, "too deep");
        let mut huge = Compound::new();
        huge.insert("x", Tag::ByteArray(vec![0; MAX_DATA_BYTES + 1]));
        assert_eq!(encode_data(&huge), None, "too big to store");
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_foreign_database_is_left_alone() {
        let dir = TestDir::new();
        let path = dir.path().join("other.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE t(x)").unwrap();
        drop(conn);
        assert!(Store::open(&path).is_err());
        let conn = Connection::open(&path).unwrap();
        let tables: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tables, 1);
    }
}
