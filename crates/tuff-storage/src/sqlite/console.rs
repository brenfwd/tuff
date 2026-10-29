//! Ad hoc SQL against a running server file, for the web panel's console.
//!
//! Queries run on the server's own connection (with the default exclusive
//! locking no other connection can open the file), one job at a time on
//! its thread, so they never interleave with world reads and writes. Each
//! job installs an authorizer and a progress handler for its duration and
//! removes them before returning.
//!
//! Read jobs also set `query_only` and may only read. Write jobs run in a
//! transaction that is rolled back for a preview and committed only when
//! the affected row count still matches the preview. Neither may touch the
//! tables the server owns (it caches their rows in memory and would
//! overwrite or ignore an edit), attach files, change pragmas, create
//! triggers or temporary objects, or load extensions.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::limits::Limit;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, Statement};

use super::Db;
pub use super::reader::Cursor;
use super::reader::{Reader, Source};

/// Tables the running server reads into memory or owns outright.
pub const SERVER_TABLES: &[&str] = &[
    "meta",
    "documents",
    "properties",
    "tuff_settings",
    "ops",
    "whitelist",
    "banned_players",
    "banned_ips",
    "user_cache",
    "plugin_grants",
    "sqlar",
];

/// Pragmas that only report, allowed without an argument.
const READ_PRAGMAS: &[&str] = &[
    "table_info",
    "table_xinfo",
    "table_list",
    "index_list",
    "index_info",
    "index_xinfo",
    "foreign_key_list",
    "function_list",
    "pragma_list",
    "collation_list",
    "database_list",
    "compile_options",
    "page_count",
    "page_size",
    "freelist_count",
    "schema_version",
    "user_version",
    "application_id",
];

/// Reporting pragmas whose argument names a table or index.
const NAMED_PRAGMAS: &[&str] = &[
    "table_info",
    "table_xinfo",
    "index_list",
    "index_info",
    "index_xinfo",
    "foreign_key_list",
];

/// Functions with side effects on the file.
const DENIED_FUNCTIONS: &[&str] = &["load_extension", "tuff_entity_index_rebuild"];

/// The largest string or blob a console statement may build, so that
/// `zeroblob(1e9)` and friends fail instead of allocating.
pub const MAX_VALUE_BYTES: i32 = 64 << 20;

/// Bytes of a blob shown in a result; the rest is only counted.
pub const BLOB_PREVIEW: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Rows per page.
    pub rows: usize,
    /// Approximate result bytes per page.
    pub bytes: usize,
    pub timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            rows: 1000,
            bytes: 4 << 20,
            timeout: Duration::from_secs(2),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob { prefix: Vec<u8>, size: usize },
}

impl Cell {
    pub fn storage_class(&self) -> &'static str {
        match self {
            Cell::Null => "null",
            Cell::Integer(_) => "integer",
            Cell::Real(_) => "real",
            Cell::Text(_) => "text",
            Cell::Blob { .. } => "blob",
        }
    }

    pub(super) fn of(value: ValueRef<'_>) -> Cell {
        match value {
            ValueRef::Null => Cell::Null,
            ValueRef::Integer(v) => Cell::Integer(v),
            ValueRef::Real(v) => Cell::Real(v),
            ValueRef::Text(v) => Cell::Text(String::from_utf8_lossy(v).into_owned()),
            ValueRef::Blob(v) => Cell::Blob {
                prefix: v[..v.len().min(BLOB_PREVIEW)].to_vec(),
                size: v.len(),
            },
        }
    }

    pub(super) fn weight(&self) -> usize {
        match self {
            Cell::Text(text) => text.len() + 8,
            Cell::Blob { prefix, .. } => prefix.len() * 2 + 16,
            _ => 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub name: String,
    pub decl_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryOutput {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Cell>>,
    /// More rows follow this page.
    pub has_more: bool,
    /// The page stopped early at the byte cap.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStep {
    pub id: i64,
    pub parent: i64,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaColumn {
    pub name: String,
    pub decl_type: String,
    pub pk: bool,
    pub not_null: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaObject {
    pub name: String,
    /// `table` or `view`.
    pub kind: String,
    pub columns: Vec<SchemaColumn>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteAction {
    /// Run, count and roll back.
    Preview,
    /// Run and commit if exactly `expect` rows change, else roll back.
    Commit { expect: u64 },
}

/// A chunk whose stored rows a statement changed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChunkKey {
    pub dimension: String,
    pub x: i32,
    pub z: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOutput {
    pub affected: u64,
    pub committed: bool,
    /// Chunks whose `chunks`, `entities` or `pois` rows changed.
    pub touched: Vec<ChunkKey>,
}

/// Why a statement was refused or failed; the text is shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleError {
    /// The statement failed or was not allowed.
    Sql(String),
    /// A commit found a different row count than its preview.
    Changed { expected: u64, actual: u64 },
    /// The world file is not open.
    Closed,
}

impl std::fmt::Display for ConsoleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConsoleError::Sql(text) => f.write_str(text),
            ConsoleError::Changed { expected, actual } => write!(
                f,
                "the statement now changes {actual} rows instead of {expected}; nothing was committed"
            ),
            ConsoleError::Closed => f.write_str("the server file is closed"),
        }
    }
}

impl std::error::Error for ConsoleError {}

type Result<T> = std::result::Result<T, ConsoleError>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Read,
    Write,
}

/// Tables of per-chunk world data. Statements may change their rows, but
/// the web layer first checks that no touched chunk is loaded.
pub const WORLD_TABLES: &[&str] = &["chunks", "entities", "pois"];

const TOUCHED: &str = "_tuff_touched";
const TOUCH_TRIGGER: &str = "_tuff_touch_";
const MAX_TOUCHED: usize = 10_000;

fn world_table(table: &str) -> bool {
    WORLD_TABLES.contains(&table.to_ascii_lowercase().as_str())
}

fn protected(table: &str) -> bool {
    SERVER_TABLES.contains(&table.to_ascii_lowercase().as_str())
}

fn decide(mode: Mode, ctx: &AuthContext<'_>, denied: &mut Option<String>) -> Authorization {
    let main_or_none = |db: Option<&str>| db.is_none_or(|db| db == "main");
    let refusal = match ctx.action {
        AuthAction::Select | AuthAction::Read { .. } | AuthAction::Recursive => None,
        AuthAction::Function { function_name } => DENIED_FUNCTIONS
            .iter()
            .any(|f| f.eq_ignore_ascii_case(function_name))
            .then(|| format!("the function {function_name} is not allowed here")),
        AuthAction::Pragma {
            pragma_name,
            pragma_value,
        } => (pragma_value.is_some()
            && !NAMED_PRAGMAS
                .iter()
                .any(|p| p.eq_ignore_ascii_case(pragma_name))
            || !READ_PRAGMAS
                .iter()
                .any(|p| p.eq_ignore_ascii_case(pragma_name)))
        .then(|| format!("PRAGMA {pragma_name} is not allowed here")),
        AuthAction::Insert { table_name }
            if mode == Mode::Write
                && table_name == TOUCHED
                && ctx.accessor.is_some_and(|a| a.starts_with(TOUCH_TRIGGER)) =>
        {
            None
        }
        AuthAction::CreateTable { table_name }
        | AuthAction::DropTable { table_name }
        | AuthAction::CreateIndex { table_name, .. }
        | AuthAction::DropIndex { table_name, .. }
        | AuthAction::AlterTable { table_name, .. }
        | AuthAction::Analyze { table_name }
            if mode == Mode::Write && world_table(table_name) =>
        {
            Some(format!(
                "{table_name} belongs to the running server; only its rows can be changed"
            ))
        }
        AuthAction::Insert { table_name }
        | AuthAction::Delete { table_name }
        | AuthAction::Update { table_name, .. }
        | AuthAction::CreateTable { table_name }
        | AuthAction::DropTable { table_name }
        | AuthAction::CreateIndex { table_name, .. }
        | AuthAction::DropIndex { table_name, .. }
        | AuthAction::AlterTable { table_name, .. }
        | AuthAction::Analyze { table_name } => match mode {
            Mode::Read => Some("this connection is read-only".to_owned()),
            Mode::Write if !main_or_none(ctx.database_name) => {
                Some("only the main database can be changed".to_owned())
            }
            Mode::Write if protected(table_name) => Some(format!(
                "{table_name} belongs to the running server, which keeps its rows in memory; \
                 stop the server to edit it"
            )),
            Mode::Write => None,
        },
        AuthAction::CreateView { .. } | AuthAction::DropView { .. } => match mode {
            Mode::Read => Some("this connection is read-only".to_owned()),
            Mode::Write => None,
        },
        AuthAction::Attach { .. } | AuthAction::Detach { .. } => {
            Some("ATTACH and DETACH are not allowed".to_owned())
        }
        AuthAction::Transaction { .. } | AuthAction::Savepoint { .. } => {
            Some("the console manages transactions itself".to_owned())
        }
        AuthAction::CreateTrigger { .. }
        | AuthAction::DropTrigger { .. }
        | AuthAction::CreateTempTrigger { .. }
        | AuthAction::DropTempTrigger { .. } => Some("triggers are not allowed".to_owned()),
        _ => Some("this kind of statement is not allowed here".to_owned()),
    };
    match refusal {
        None => Authorization::Allow,
        Some(text) => {
            denied.get_or_insert(text);
            Authorization::Deny
        }
    }
}

/// Installs the authorizer and time limit for one job and removes them,
/// and the read-only pragma, when dropped.
pub(super) struct Guard<'c> {
    conn: &'c Connection,
    denied: Arc<std::sync::Mutex<Option<String>>>,
    timed_out: Arc<AtomicBool>,
    cancel: Cancel,
    deadline: Arc<std::sync::Mutex<Instant>>,
    mode: Mode,
    length: i32,
}

/// Stops a running statement from another thread.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

impl<'c> Guard<'c> {
    pub(super) fn install(
        conn: &'c Connection,
        mode: Mode,
        limits: Limits,
        cancel: Cancel,
    ) -> Result<Self> {
        let denied = Arc::new(std::sync::Mutex::new(None));
        let timed_out = Arc::new(AtomicBool::new(false));
        let deadline = Arc::new(std::sync::Mutex::new(Instant::now() + limits.timeout));
        let guard = Guard {
            conn,
            denied: denied.clone(),
            timed_out: timed_out.clone(),
            cancel: cancel.clone(),
            deadline: deadline.clone(),
            mode,
            length: conn
                .set_limit(Limit::SQLITE_LIMIT_LENGTH, MAX_VALUE_BYTES)
                .map_err(sql)?,
        };
        if mode == Mode::Read {
            conn.pragma_update(None, "query_only", true).map_err(sql)?;
        }
        conn.authorizer(Some(move |ctx: AuthContext<'_>| {
            let mut denied = denied.lock().unwrap_or_else(|e| e.into_inner());
            decide(mode, &ctx, &mut denied)
        }))
        .map_err(sql)?;
        conn.progress_handler(
            1000,
            Some(move || {
                let end = *deadline.lock().unwrap_or_else(|e| e.into_inner());
                let late = Instant::now() >= end;
                if late {
                    timed_out.store(true, Ordering::Relaxed);
                }
                late || cancel.is_cancelled()
            }),
        )
        .map_err(sql)?;
        Ok(guard)
    }

    /// Starts a fresh time limit, for the next page of a cursor.
    pub(super) fn rearm(&self, timeout: Duration) {
        *self.deadline.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now() + timeout;
        self.timed_out.store(false, Ordering::Relaxed);
    }

    pub(super) fn explain(&self, err: rusqlite::Error) -> ConsoleError {
        if self.cancel.is_cancelled() {
            return ConsoleError::Sql("the statement was cancelled".into());
        }
        if self.timed_out.load(Ordering::Relaxed) {
            return ConsoleError::Sql("the statement ran past the time limit".into());
        }
        let denied = self.denied.lock().unwrap_or_else(|e| e.into_inner()).take();
        match denied {
            Some(text) => ConsoleError::Sql(text),
            None => sql(err),
        }
    }

    pub(super) fn prepare(&self, text: &str) -> Result<Statement<'c>> {
        if text.trim().is_empty() {
            return Err(ConsoleError::Sql("the statement is empty".into()));
        }
        self.conn.prepare(text).map_err(|err| match err {
            rusqlite::Error::MultipleStatement => {
                ConsoleError::Sql("run one statement at a time".into())
            }
            err => self.explain(err),
        })
    }
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        let _ = self
            .conn
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
        let _ = self.conn.progress_handler(0, None::<fn() -> bool>);
        let _ = self.conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, self.length);
        if self.mode == Mode::Read {
            let _ = self.conn.pragma_update(None, "query_only", false);
        }
    }
}

fn sql(err: rusqlite::Error) -> ConsoleError {
    ConsoleError::Sql(err.to_string())
}

/// Runs one read-only statement and returns the rows from `offset` on, at
/// most `limit` of them (capped by `limits`).
pub fn query(
    conn: &Connection,
    text: &str,
    offset: usize,
    limit: usize,
    limits: Limits,
) -> Result<QueryOutput> {
    query_with(conn, text, offset, limit, limits, Cancel::new())
}

pub fn query_with(
    conn: &Connection,
    text: &str,
    offset: usize,
    limit: usize,
    limits: Limits,
    cancel: Cancel,
) -> Result<QueryOutput> {
    let guard = Guard::install(conn, Mode::Read, limits, cancel)?;
    let mut stmt = guard.prepare(text)?;
    if !stmt.readonly() {
        return Err(ConsoleError::Sql("this connection is read-only".into()));
    }
    let columns = stmt
        .columns()
        .iter()
        .map(|c| Column {
            name: c.name().to_owned(),
            decl_type: c.decl_type().map(str::to_owned),
        })
        .collect::<Vec<_>>();
    let limit = limit.clamp(1, limits.rows.max(1));
    let mut rows = stmt.query([]).map_err(|e| guard.explain(e))?;
    let mut out = QueryOutput {
        columns,
        rows: Vec::new(),
        has_more: false,
        truncated: false,
    };
    let mut skipped = 0;
    let mut bytes = 0;
    while let Some(row) = rows.next().map_err(|e| guard.explain(e))? {
        if skipped < offset {
            skipped += 1;
            continue;
        }
        if out.rows.len() == limit {
            out.has_more = true;
            break;
        }
        let cells = (0..out.columns.len())
            .map(|i| row.get_ref(i).map(Cell::of))
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(sql)?;
        bytes += cells.iter().map(Cell::weight).sum::<usize>();
        out.rows.push(cells);
        if bytes >= limits.bytes {
            out.truncated = true;
            out.has_more = true;
            break;
        }
    }
    Ok(out)
}

/// The query plan of one read-only statement.
pub fn explain(conn: &Connection, text: &str, limits: Limits) -> Result<Vec<PlanStep>> {
    explain_with(conn, text, limits, Cancel::new())
}

pub fn explain_with(
    conn: &Connection,
    text: &str,
    limits: Limits,
    cancel: Cancel,
) -> Result<Vec<PlanStep>> {
    let guard = Guard::install(conn, Mode::Read, limits, cancel)?;
    let stmt = guard.prepare(text)?;
    if !stmt.readonly() {
        return Err(ConsoleError::Sql("this connection is read-only".into()));
    }
    drop(stmt);
    let mut plan = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {text}"))
        .map_err(|e| guard.explain(e))?;
    let steps = plan
        .query_map([], |row| {
            Ok(PlanStep {
                id: row.get(0)?,
                parent: row.get(1)?,
                detail: row.get(3)?,
            })
        })
        .map_err(|e| guard.explain(e))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| guard.explain(e))?;
    Ok(steps)
}

/// Every table and view, temporary ones (the query views) included.
pub fn schema(conn: &Connection) -> Result<Vec<SchemaObject>> {
    let mut objects = conn
        .prepare(
            "SELECT name, type FROM sqlite_temp_schema WHERE type IN ('table', 'view') \
             UNION ALL SELECT name, type FROM main.sqlite_schema \
             WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY 1",
        )
        .map_err(sql)?
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get(1)?)))
        .map_err(sql)?
        .collect::<rusqlite::Result<Vec<(String, String)>>>()
        .map_err(sql)?;
    objects.dedup_by(|a, b| a.0 == b.0);
    let mut info = conn
        .prepare("SELECT name, type, \"notnull\", pk FROM pragma_table_info(?1)")
        .map_err(sql)?;
    objects
        .into_iter()
        .map(|(name, kind)| {
            let columns = info
                .query_map([&name], |row| {
                    Ok(SchemaColumn {
                        name: row.get(0)?,
                        decl_type: row.get(1)?,
                        not_null: row.get::<_, i64>(2)? != 0,
                        pk: row.get::<_, i64>(3)? != 0,
                    })
                })
                .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
                .unwrap_or_default();
            Ok(SchemaObject {
                name,
                kind,
                columns,
            })
        })
        .collect()
}

fn tracked_tables(tx: &rusqlite::Transaction<'_>) -> Result<Vec<&'static str>> {
    let mut found = Vec::new();
    for table in WORLD_TABLES {
        let present = tx
            .query_row(
                "SELECT 1 FROM main.sqlite_schema WHERE type = 'table' AND name = ?1",
                [table],
                |_| Ok(()),
            )
            .is_ok();
        if present {
            found.push(*table);
        }
    }
    Ok(found)
}

fn start_tracking(tx: &rusqlite::Transaction<'_>, tables: &[&str]) -> Result<()> {
    let mut batch = format!(
        "CREATE TEMP TABLE {TOUCHED}(dimension TEXT NOT NULL, x INTEGER NOT NULL, z INTEGER NOT NULL);"
    );
    for table in tables {
        for (event, keys) in [
            ("INSERT", vec!["new"]),
            ("UPDATE", vec!["old", "new"]),
            ("DELETE", vec!["old"]),
        ] {
            let body: String = keys
                .iter()
                .map(|row| {
                    format!("INSERT INTO {TOUCHED} VALUES ({row}.dimension, {row}.x, {row}.z);")
                })
                .collect();
            batch.push_str(&format!(
                "CREATE TEMP TRIGGER {TOUCH_TRIGGER}{table}_{event} AFTER {event} ON main.{table} \
                 BEGIN {body} END;"
            ));
        }
    }
    tx.execute_batch(&batch).map_err(sql)
}

/// The touched chunks, and how many bookkeeping rows the triggers wrote.
fn finish_tracking(
    tx: &rusqlite::Transaction<'_>,
    tables: &[&str],
) -> Result<(Vec<ChunkKey>, u64)> {
    let logged: i64 = tx
        .query_row(&format!("SELECT count(*) FROM {TOUCHED}"), [], |r| r.get(0))
        .map_err(sql)?;
    let mut keys = tx
        .prepare(&format!(
            "SELECT DISTINCT dimension, x, z FROM {TOUCHED} LIMIT {}",
            MAX_TOUCHED + 1
        ))
        .map_err(sql)?
        .query_map([], |row| {
            Ok(ChunkKey {
                dimension: row.get(0)?,
                x: row.get(1)?,
                z: row.get(2)?,
            })
        })
        .map_err(sql)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(sql)?;
    let mut drop = String::new();
    for table in tables {
        for event in ["INSERT", "UPDATE", "DELETE"] {
            drop.push_str(&format!(
                "DROP TRIGGER temp.{TOUCH_TRIGGER}{table}_{event};"
            ));
        }
    }
    drop.push_str(&format!("DROP TABLE temp.{TOUCHED};"));
    tx.execute_batch(&drop).map_err(sql)?;
    if keys.len() > MAX_TOUCHED {
        return Err(ConsoleError::Sql(format!(
            "the statement changes more than {MAX_TOUCHED} chunks; narrow it down"
        )));
    }
    keys.sort();
    Ok((keys, logged as u64))
}

/// Runs one changing statement in a transaction, then rolls it back or
/// commits it as `action` says. With `checked`, a commit also needs every
/// chunk the statement touches to be among them.
pub fn write(
    conn: &mut Connection,
    text: &str,
    action: WriteAction,
    limits: Limits,
) -> Result<WriteOutput> {
    write_checked(conn, text, action, limits, None)
}

pub fn write_checked(
    conn: &mut Connection,
    text: &str,
    action: WriteAction,
    limits: Limits,
    checked: Option<&std::collections::HashSet<ChunkKey>>,
) -> Result<WriteOutput> {
    write_checked_with(conn, text, action, limits, checked, None)
}

pub(super) fn write_checked_with(
    conn: &mut Connection,
    text: &str,
    action: WriteAction,
    limits: Limits,
    checked: Option<&std::collections::HashSet<ChunkKey>>,
    before_commit: Option<&super::Applier>,
) -> Result<WriteOutput> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(sql)?;
    let tables = tracked_tables(&tx)?;
    start_tracking(&tx, &tables)?;
    let affected = {
        let guard = Guard::install(&tx, Mode::Write, limits, Cancel::new())?;
        let mut stmt = guard.prepare(text)?;
        let before = tx.total_changes();
        let mut rows = stmt.raw_query();
        while rows.next().map_err(|e| guard.explain(e))?.is_some() {}
        drop(rows);
        tx.total_changes() - before
    };
    let (touched, logged) = finish_tracking(&tx, &tables)?;
    let affected = affected - logged;
    match action {
        WriteAction::Commit { expect } if expect == affected => {
            if let Some(checked) = checked
                && !touched.iter().all(|key| checked.contains(key))
            {
                return Err(ConsoleError::Sql(
                    "the statement now touches chunks that were not checked; nothing was committed"
                        .into(),
                ));
            }
            if let Some(apply) = before_commit {
                apply(&tx, &touched).map_err(|err| ConsoleError::Sql(err.to_string()))?;
            }
            tx.commit().map_err(sql)?;
            Ok(WriteOutput {
                affected,
                committed: true,
                touched,
            })
        }
        WriteAction::Commit { expect } => Err(ConsoleError::Changed {
            expected: expect,
            actual: affected,
        }),
        WriteAction::Preview => Ok(WriteOutput {
            affected,
            committed: false,
            touched,
        }),
    }
}

/// The console for one server file. Reads run on their own read-only
/// connection (or, under exclusive locking, on a snapshot copy); writes
/// run on the file's thread.
#[derive(Clone)]
pub struct SqlConsole {
    pub(super) db: Option<Db>,
    pub(super) source: Source,
    pub(super) apply: Option<super::Applier>,
}

impl SqlConsole {
    fn run<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let Some(db) = &self.db else {
            return Err(ConsoleError::Closed);
        };
        db.run_io(move |conn| Ok(job(conn)))
            .unwrap_or_else(|_: io::Error| Err(ConsoleError::Closed))
    }

    fn reader(&self) -> Result<Option<Reader>> {
        self.source
            .open()
            .map_err(|err| ConsoleError::Sql(err.to_string()))
    }

    /// A read-only console over a world kept in another layout, imported
    /// into a temporary SQLite file whenever a fresh view is needed.
    pub fn importing(build: super::Importer, apply: super::Applier) -> Self {
        Self {
            db: None,
            source: Source::import(build),
            apply: Some(apply),
        }
    }

    /// Whether statements that change data are accepted.
    pub fn can_write(&self) -> bool {
        self.db.is_some() || self.apply.is_some()
    }

    /// Whether reads leave the file's thread alone.
    pub fn reads_apart(&self) -> bool {
        !matches!(self.source, Source::Shared)
    }

    pub fn query(
        &self,
        text: String,
        offset: usize,
        limit: usize,
        limits: Limits,
        cancel: Cancel,
    ) -> Result<QueryOutput> {
        match self.reader()? {
            Some(reader) => query_with(&reader.conn, &text, offset, limit, limits, cancel),
            None => self.run(move |conn| query_with(conn, &text, offset, limit, limits, cancel)),
        }
    }

    /// Opens a statement whose pages are fetched one at a time from a
    /// consistent view of the file.
    pub fn cursor(&self, text: String, limits: Limits, cancel: Cancel) -> Result<Cursor> {
        let Some(reader) = self.reader()? else {
            return Err(ConsoleError::Sql(
                "this server file cannot stream results".into(),
            ));
        };
        Ok(Cursor::start(reader, text, limits, cancel))
    }

    pub fn explain(&self, text: String, limits: Limits, cancel: Cancel) -> Result<Vec<PlanStep>> {
        match self.reader()? {
            Some(reader) => explain_with(&reader.conn, &text, limits, cancel),
            None => self.run(move |conn| explain_with(conn, &text, limits, cancel)),
        }
    }

    pub fn schema(&self) -> Result<Vec<SchemaObject>> {
        match self.reader()? {
            Some(reader) => schema(&reader.conn),
            None => self.run(|conn| schema(conn)),
        }
    }

    pub fn write(&self, text: String, action: WriteAction, limits: Limits) -> Result<WriteOutput> {
        self.write_checked(text, action, limits, None)
    }

    pub fn write_checked(
        &self,
        text: String,
        action: WriteAction,
        limits: Limits,
        checked: Option<std::collections::HashSet<ChunkKey>>,
    ) -> Result<WriteOutput> {
        if self.db.is_none() {
            let (Source::Import { build, .. }, Some(apply)) = (&self.source, &self.apply) else {
                return Err(ConsoleError::Closed);
            };
            let snap = super::reader::SnapFile::import(build)
                .map_err(|err| ConsoleError::Sql(err.to_string()))?;
            let mut conn = Connection::open(snap.path()).map_err(sql)?;
            tuff_sqlite_nbt::register(&conn).map_err(sql)?;
            let schema_version = |conn: &Connection| -> rusqlite::Result<i64> {
                conn.query_row("PRAGMA main.schema_version", [], |r| r.get(0))
            };
            let before = schema_version(&conn).map_err(sql)?;
            let guarded: super::Applier = {
                let apply = apply.clone();
                Arc::new(move |tx, keys| {
                    if schema_version(tx).map_err(io::Error::other)? != before {
                        return Err(io::Error::other(
                            "schema changes cannot be saved to a region-file world",
                        ));
                    }
                    apply(tx, keys)
                })
            };
            let out = write_checked_with(
                &mut conn,
                &text,
                action,
                limits,
                checked.as_ref(),
                Some(&guarded),
            );
            drop(conn);
            drop(snap);
            self.source.invalidate();
            return out;
        }
        let out =
            self.run(move |conn| write_checked(conn, &text, action, limits, checked.as_ref()))?;
        self.source.invalidate();
        Ok(out)
    }
}

#[cfg(test)]
#[path = "console_tests.rs"]
mod tests;
