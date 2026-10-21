//! The SQL console: read-only queries against a SQLite world, and, behind
//! a permission, a setting and a per-tab toggle, previewed writes.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use hyper::{Method, StatusCode};
use serde_json::{Value, json};

use super::api::{json_body, method_not_allowed, require};
use super::audit::Entry;
use super::auth::{AuthContext, Scope};
use super::guard::{self, error};
use super::host::{HostFuture, PanelHost};
use tuff_http::{Request, Response};
use tuff_storage::sqlite::console::{
    Cancel, Cell, ChunkKey, ConsoleError, Cursor, Limits, QueryOutput, SqlConsole, WriteAction,
};

pub const ALLOW_WRITE_KEY: &str = "tuff.web.sql.allowWrite";
pub const TIMEOUT_KEY: &str = "tuff.web.sql.timeoutSeconds";
pub const DEFAULT_TIMEOUT_SECONDS: i64 = 60;
/// What the SQL console needs from the server.
pub trait SqlHost: Send + Sync {
    /// The world backend's console, if it has one.
    fn console(&self) -> Option<SqlConsole>;
    fn backend_name(&self) -> String;
    /// Which of `touched` the server has loaded.
    fn loaded_among<'a>(&'a self, touched: &'a [ChunkKey]) -> HostFuture<'a, Vec<ChunkKey>>;
}

const SAVED_FILE: &str = "web/sql-queries.json";
const SAVED_MAX_BYTES: usize = 1 << 20;
const SAVED_PER_USER: usize = 200;
const SQL_MAX: usize = 64 << 10;
const PAGE: usize = 100;

fn export_limits() -> Limits {
    Limits {
        rows: 100_000,
        bytes: 32 << 20,
        ..Limits::default()
    }
}

type Principal = (usize, String);

fn principal(shared: &dyn PanelHost, ctx: &AuthContext) -> Principal {
    (
        shared as *const dyn PanelHost as *const () as usize,
        ctx.principal.name(),
    )
}

/// Principals with a statement running, one at a time each, and the way to
/// stop it.
fn running() -> &'static Mutex<HashMap<Principal, Cancel>> {
    static RUNNING: OnceLock<Mutex<HashMap<Principal, Cancel>>> = OnceLock::new();
    RUNNING.get_or_init(Default::default)
}

struct Running(Principal);

impl Running {
    fn claim(key: Principal, cancel: Cancel) -> Option<Running> {
        let mut map = running().lock().unwrap_or_else(|e| e.into_inner());
        if map.contains_key(&key) {
            return None;
        }
        map.insert(key.clone(), cancel);
        Some(Running(key))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        running()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

struct OpenCursor {
    id: u64,
    cursor: Arc<Cursor>,
    cancel: Cancel,
}

/// The statement each principal can still page through; a new query
/// replaces it, and the cursor closes itself when idle.
fn cursors() -> &'static Mutex<HashMap<Principal, OpenCursor>> {
    static CURSORS: OnceLock<Mutex<HashMap<Principal, OpenCursor>>> = OnceLock::new();
    CURSORS.get_or_init(Default::default)
}

fn next_cursor_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn timeout(shared: &dyn PanelHost) -> Duration {
    let seconds = shared
        .config_int(TIMEOUT_KEY)
        .unwrap_or(DEFAULT_TIMEOUT_SECONDS)
        .clamp(1, 3600);
    Duration::from_secs(seconds as u64)
}

fn limits(shared: &dyn PanelHost) -> Limits {
    Limits {
        timeout: timeout(shared),
        ..Limits::default()
    }
}

fn writes_allowed(shared: &dyn PanelHost, ctx: &AuthContext) -> bool {
    ctx.allows(Scope::SqlWrite) && shared.config_bool(ALLOW_WRITE_KEY) == Some(true)
}

pub async fn handle(shared: Arc<dyn PanelHost>, req: Request) -> Response {
    let ctx = match require(&req, Some(Scope::SqlRead)) {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    let rest = req.rest.trim_matches('/').to_owned();
    let console = shared.sql().console();
    if rest.is_empty() {
        if req.method != Method::GET {
            return method_not_allowed();
        }
        let limits = limits(&*shared);
        let mut state = json!({
            "available": console.is_some(),
            "backend": shared.sql().backend_name(),
            "canWrite": console.as_ref().is_some_and(|c| c.can_write()) && writes_allowed(&*shared, &ctx),
            "limits": {
                "rows": limits.rows,
                "bytes": limits.bytes,
                "timeoutMs": limits.timeout.as_millis() as u64,
            },
        });
        if console.is_none() {
            state["reason"] = json!("this world backend has no SQL console");
        }
        return Response::json(StatusCode::OK, &state);
    }
    if rest == "saved" {
        return saved(&*shared, &req, &ctx).await;
    }
    let Some(console) = console else {
        return error(
            StatusCode::NOT_FOUND,
            "this world backend has no SQL console",
        );
    };
    if rest == "schema" {
        if req.method != Method::GET {
            return method_not_allowed();
        }
        return match blocking(move || console.schema()).await {
            Ok(objects) => Response::json(StatusCode::OK, &schema_json(&objects)),
            Err(err) => error(StatusCode::BAD_REQUEST, &err.to_string()),
        };
    }
    if req.method != Method::POST {
        return method_not_allowed();
    }
    let body = match json_body(&req) {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    if rest == "cancel" {
        let cancel = running()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&principal(&*shared, &ctx))
            .cloned();
        if let Some(cancel) = &cancel {
            cancel.cancel();
        }
        return Response::json(StatusCode::OK, &json!({ "cancelled": cancel.is_some() }));
    }
    let cursor_id = (rest == "query")
        .then(|| body.get("cursor").and_then(Value::as_u64))
        .flatten();
    let sql = match body.get("sql").and_then(Value::as_str) {
        Some(sql) => sql.to_owned(),
        None if cursor_id.is_some() => String::new(),
        None => return error(StatusCode::BAD_REQUEST, "missing sql"),
    };
    if sql.len() > SQL_MAX {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "the statement is too long");
    }
    let action = match rest.as_str() {
        "query" => "sql.query",
        "explain" => "sql.explain",
        "export" => "sql.export",
        "write" => match body.get("action").and_then(Value::as_str) {
            Some("preview") => "sql.write.preview",
            Some("commit") => "sql.write.commit",
            Some("rollback") => "sql.write.rollback",
            _ => return error(StatusCode::BAD_REQUEST, "unknown write action"),
        },
        _ => return error(StatusCode::NOT_FOUND, "no such endpoint"),
    };
    let entry = Entry::new(ctx.principal.name(), action)
        .remote(guard::client_ip(&*shared, &req))
        .detail(sql.clone());
    if rest == "write" && !writes_allowed(&*shared, &ctx) {
        shared.web().audit.record(entry.outcome("denied"));
        let why = if ctx.allows(Scope::SqlWrite) {
            "writes are off; set tuff.web.sql.allowWrite to true to allow them"
        } else {
            "not allowed"
        };
        return error(StatusCode::FORBIDDEN, why);
    }
    if action == "sql.write.rollback" {
        shared.web().audit.record(entry);
        return Response::json(StatusCode::OK, &json!({ "rolledBack": true }));
    }
    let key = principal(&*shared, &ctx);
    let cancel = match cursor_id {
        Some(id) => {
            let map = cursors().lock().unwrap_or_else(|e| e.into_inner());
            match map.get(&key).filter(|open| open.id == id) {
                Some(open) => open.cancel.clone(),
                None => {
                    return error(
                        StatusCode::GONE,
                        "this result has expired; run the statement again",
                    );
                }
            }
        }
        None => Cancel::new(),
    };
    let Some(claim) = Running::claim(key.clone(), cancel.clone()) else {
        shared.web().audit.record(entry.outcome("busy"));
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "another statement of yours is still running",
        );
    };
    let started = Instant::now();
    let job = Job {
        shared: shared.clone(),
        console,
        key,
        cancel,
        limits: limits(&*shared),
    };
    let result = run(job, rest.as_str(), sql, cursor_id, &body).await;
    drop(claim);
    let elapsed = started.elapsed().as_millis() as u64;
    match result {
        Ok(Done::Json(mut value)) => {
            if cursor_id.is_none() {
                shared.web().audit.record(entry);
            }
            value["elapsedMs"] = json!(elapsed);
            Response::json(StatusCode::OK, &value)
        }
        Ok(Done::File(mut resp)) => {
            shared.web().audit.record(entry);
            resp.set_header("cache-control", "no-store");
            resp
        }
        Err(err) => {
            shared.web().audit.record(entry.outcome(err.to_string()));
            let status = match err {
                ConsoleError::Changed { .. } => StatusCode::CONFLICT,
                ConsoleError::Closed => StatusCode::SERVICE_UNAVAILABLE,
                ConsoleError::Sql(_) => StatusCode::BAD_REQUEST,
            };
            error(status, &err.to_string())
        }
    }
}

enum Done {
    Json(Value),
    File(Response),
}

async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, ConsoleError> + Send + 'static,
) -> Result<T, ConsoleError> {
    tokio::task::spawn_blocking(job)
        .await
        .unwrap_or(Err(ConsoleError::Closed))
}

struct Job {
    shared: Arc<dyn PanelHost>,
    console: SqlConsole,
    key: Principal,
    cancel: Cancel,
    limits: Limits,
}

async fn run(
    job: Job,
    endpoint: &str,
    sql: String,
    cursor_id: Option<u64>,
    body: &Value,
) -> Result<Done, ConsoleError> {
    let Job {
        shared,
        console,
        key,
        cancel,
        limits,
    } = job;
    let number = |key: &str| body.get(key).and_then(Value::as_u64);
    match endpoint {
        "query" => {
            let limit = number("limit").unwrap_or(PAGE as u64) as usize;
            let (id, cursor) = match cursor_id {
                Some(id) => {
                    let map = cursors().lock().unwrap_or_else(|e| e.into_inner());
                    let open = map.get(&key).filter(|open| open.id == id);
                    (id, open.map(|open| open.cursor.clone()))
                }
                None => {
                    let opened = {
                        let console = console.clone();
                        let cancel = cancel.clone();
                        blocking(move || console.cursor(sql, limits, cancel)).await?
                    };
                    (next_cursor_id(), Some(Arc::new(opened)))
                }
            };
            let cursor = cursor.ok_or_else(|| ConsoleError::Sql("the result expired".into()))?;
            let page = {
                let cursor = cursor.clone();
                blocking(move || cursor.fetch(limit, limits)).await
            };
            let mut cursors = cursors().lock().unwrap_or_else(|e| e.into_inner());
            let out = match page {
                Ok(out) => out,
                Err(err) => {
                    cursors.remove(&key);
                    return Err(err);
                }
            };
            let mut value = page_json(&out);
            if out.has_more {
                if cursor_id.is_none() {
                    cursors.insert(key, OpenCursor { id, cursor, cancel });
                }
                value["cursor"] = json!(id);
            } else {
                cursors.remove(&key);
            }
            Ok(Done::Json(value))
        }
        "explain" => {
            let plan = blocking(move || console.explain(sql, limits, cancel)).await?;
            let steps: Vec<Value> = plan
                .iter()
                .map(|s| json!({ "id": s.id, "parent": s.parent, "detail": s.detail }))
                .collect();
            Ok(Done::Json(json!({ "plan": steps })))
        }
        "export" => {
            let csv = body.get("format").and_then(Value::as_str) != Some("json");
            let limits = Limits {
                timeout: limits.timeout,
                ..export_limits()
            };
            let out = blocking(move || console.query(sql, 0, limits.rows, limits, cancel)).await?;
            let (bytes, kind, name) = if csv {
                (to_csv(&out), "text/csv; charset=utf-8", "query.csv")
            } else {
                (
                    serde_json::to_vec(&to_objects(&out)).unwrap_or_default(),
                    "application/json",
                    "query.json",
                )
            };
            let mut resp = Response::new(StatusCode::OK, bytes);
            resp.set_header("content-type", kind);
            crate::upload::attachment(&mut resp, name);
            Ok(Done::File(resp))
        }
        _ => {
            let action = match body.get("action").and_then(Value::as_str) {
                Some("commit") => WriteAction::Commit {
                    expect: number("expect").ok_or_else(|| {
                        ConsoleError::Sql("a commit needs the previewed row count".into())
                    })?,
                },
                _ => WriteAction::Preview,
            };
            let out = write_world(&shared, console, sql, action, limits).await?;
            let mut value = json!({
                "affected": out.affected,
                "committed": out.committed,
                "chunks": out.touched.len(),
            });
            if let Some(warning) = out.warning {
                value["warning"] = json!(warning);
            }
            Ok(Done::Json(value))
        }
    }
}

struct WorldWrite {
    affected: u64,
    committed: bool,
    touched: Vec<ChunkKey>,
    warning: Option<String>,
}

fn loaded_refusal(loaded: &[ChunkKey]) -> ConsoleError {
    let shown: Vec<String> = loaded
        .iter()
        .take(5)
        .map(|k| format!("{} {} {}", k.dimension, k.x, k.z))
        .collect();
    let more = loaded.len().saturating_sub(shown.len());
    let tail = if more > 0 {
        format!(" and {more} more")
    } else {
        String::new()
    };
    ConsoleError::Sql(format!(
        "the statement changes chunks the server has loaded ({}{tail}); a save would overwrite \
         the edit. Move players away and wait for them to unload, or stop the server",
        shown.join(", ")
    ))
}

/// A statement that changes stored world rows: refused while any chunk it
/// touches is loaded, and committed while saving is held so nothing
/// overwrites it.
async fn write_world(
    shared: &Arc<dyn PanelHost>,
    console: SqlConsole,
    sql: String,
    action: WriteAction,
    limits: Limits,
) -> Result<WorldWrite, ConsoleError> {
    let preview = {
        let console = console.clone();
        let sql = sql.clone();
        blocking(move || console.write(sql, WriteAction::Preview, limits)).await?
    };
    let touched = preview.touched;
    if !touched.is_empty() {
        let loaded = shared.sql().loaded_among(&touched).await;
        if !loaded.is_empty() {
            return Err(loaded_refusal(&loaded));
        }
    }
    let WriteAction::Commit { .. } = action else {
        return Ok(WorldWrite {
            affected: preview.affected,
            committed: false,
            touched,
            warning: None,
        });
    };
    let hold = if touched.is_empty() {
        None
    } else {
        let hold = shared
            .clone()
            .hold_saves("the SQL console", Duration::from_secs(120))
            .await
            .map_err(ConsoleError::Sql)?;
        let loaded = shared.sql().loaded_among(&touched).await;
        if !loaded.is_empty() {
            return Err(loaded_refusal(&loaded));
        }
        Some(hold)
    };
    let checked = (!touched.is_empty()).then(|| touched.iter().cloned().collect());
    let out = {
        let console = console.clone();
        blocking(move || console.write_checked(sql, action, limits, checked)).await?
    };
    let warning = if touched.is_empty() {
        None
    } else {
        let loaded = shared.sql().loaded_among(&touched).await;
        (!loaded.is_empty()).then(|| {
            "some edited chunks were loaded while the edit committed; run /save-all only after \
             checking them, or restart"
                .to_owned()
        })
    };
    drop(hold);
    Ok(WorldWrite {
        affected: out.affected,
        committed: out.committed,
        touched,
        warning,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

pub fn cell_json(cell: &Cell) -> Value {
    match cell {
        Cell::Null => Value::Null,
        Cell::Integer(v) => json!(v),
        Cell::Real(v) => json!(v),
        Cell::Text(v) => json!(v),
        Cell::Blob { prefix, size } => json!({ "blob": hex(prefix), "size": size }),
    }
}

fn page_json(out: &QueryOutput) -> Value {
    let columns: Vec<Value> = out
        .columns
        .iter()
        .map(|c| json!({ "name": c.name, "declType": c.decl_type }))
        .collect();
    let rows: Vec<Value> = out
        .rows
        .iter()
        .map(|r| Value::Array(r.iter().map(cell_json).collect()))
        .collect();
    let types: Vec<Value> = out
        .rows
        .iter()
        .map(|r| json!(r.iter().map(Cell::storage_class).collect::<Vec<_>>()))
        .collect();
    json!({
        "columns": columns,
        "rows": rows,
        "types": types,
        "hasMore": out.has_more,
        "truncated": out.truncated,
    })
}

fn to_objects(out: &QueryOutput) -> Value {
    Value::Array(
        out.rows
            .iter()
            .map(|row| {
                let map = out
                    .columns
                    .iter()
                    .zip(row)
                    .map(|(c, cell)| (c.name.clone(), cell_json(cell)))
                    .collect();
                Value::Object(map)
            })
            .collect(),
    )
}

fn csv_field(text: &str) -> String {
    let text = match text.chars().next() {
        Some('=' | '+' | '-' | '@' | '\t' | '\r') => format!("'{text}"),
        _ => text.to_owned(),
    };
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}

pub fn to_csv(out: &QueryOutput) -> Vec<u8> {
    let mut csv = String::new();
    let header: Vec<String> = out.columns.iter().map(|c| csv_field(&c.name)).collect();
    csv.push_str(&header.join(","));
    csv.push_str("\r\n");
    for row in &out.rows {
        let fields: Vec<String> = row
            .iter()
            .map(|cell| match cell {
                Cell::Null => String::new(),
                Cell::Integer(v) => v.to_string(),
                Cell::Real(v) => v.to_string(),
                Cell::Text(v) => csv_field(v),
                Cell::Blob { prefix, size } => format!("blob:{size}:{}", hex(prefix)),
            })
            .collect();
        csv.push_str(&fields.join(","));
        csv.push_str("\r\n");
    }
    csv.into_bytes()
}

fn schema_json(objects: &[tuff_storage::sqlite::console::SchemaObject]) -> Value {
    let objects: Vec<Value> = objects
        .iter()
        .map(|o| {
            let columns: Vec<Value> = o
                .columns
                .iter()
                .map(|c| {
                    json!({ "name": c.name, "type": c.decl_type, "pk": c.pk, "notNull": c.not_null })
                })
                .collect();
            json!({ "name": o.name, "type": o.kind, "columns": columns })
        })
        .collect();
    let functions: Vec<Value> = FUNCTIONS
        .iter()
        .map(|(name, signature, description)| {
            json!({ "name": name, "signature": signature, "description": description })
        })
        .collect();
    json!({ "objects": objects, "functions": functions })
}

const FUNCTIONS: &[(&str, &str, &str)] = &[
    ("nbt_json", "nbt_json(blob)", "The NBT as JSON"),
    (
        "nbt_try_json",
        "nbt_try_json(blob)",
        "Like nbt_json, NULL when it does not decode",
    ),
    (
        "nbt_error",
        "nbt_error(blob)",
        "Why a blob does not decode, or NULL",
    ),
    ("nbt_snbt", "nbt_snbt(blob)", "SNBT text"),
    (
        "nbt_get",
        "nbt_get(blob, path)",
        "One value by path, e.g. Pos[1]",
    ),
    (
        "block_at",
        "block_at(chunk_blob, x, y, z)",
        "The block state at a world position",
    ),
    (
        "nbt_uuid",
        "nbt_uuid(json)",
        "Hyphenated UUID from a JSON int array",
    ),
    (
        "sqlar_text",
        "sqlar_text(data, sz)",
        "An sqlar row's contents as text",
    ),
];

type SavedMap = BTreeMap<String, Vec<Value>>;

async fn load_saved(shared: &dyn PanelHost) -> SavedMap {
    let Some(files) = shared.web().files.clone() else {
        return SavedMap::new();
    };
    tokio::task::spawn_blocking(move || files.read_file(SAVED_FILE, SAVED_MAX_BYTES))
        .await
        .ok()
        .and_then(Result::ok)
        .flatten()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

async fn saved(shared: &dyn PanelHost, req: &Request, ctx: &AuthContext) -> Response {
    let user = ctx.principal.name();
    match req.method {
        Method::GET => {
            let all = load_saved(shared).await;
            let mine = all.get(&user).cloned().unwrap_or_default();
            Response::json(StatusCode::OK, &json!({ "queries": mine }))
        }
        Method::PUT => {
            let body = match json_body(req) {
                Ok(body) => body,
                Err(resp) => return resp,
            };
            let Some(list) = body.get("queries").and_then(Value::as_array) else {
                return error(StatusCode::BAD_REQUEST, "missing queries");
            };
            if list.len() > SAVED_PER_USER {
                return error(StatusCode::BAD_REQUEST, "too many saved queries");
            }
            let mut queries = Vec::with_capacity(list.len());
            for item in list {
                let name = item.get("name").and_then(Value::as_str);
                let sql = item.get("sql").and_then(Value::as_str);
                match (name, sql) {
                    (Some(name), Some(sql)) if name.len() <= 200 && sql.len() <= SQL_MAX => {
                        queries.push(json!({ "name": name, "sql": sql }));
                    }
                    _ => return error(StatusCode::BAD_REQUEST, "bad saved query"),
                }
            }
            let Some(files) = shared.web().files.clone() else {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "no storage for saved queries",
                );
            };
            let mut all = load_saved(shared).await;
            all.insert(user, queries.clone());
            let bytes = serde_json::to_vec(&all).unwrap_or_default();
            if bytes.len() > SAVED_MAX_BYTES {
                return error(StatusCode::PAYLOAD_TOO_LARGE, "saved queries are full");
            }
            match tokio::task::spawn_blocking(move || files.write_file(SAVED_FILE, &bytes)).await {
                Ok(Ok(())) => Response::json(StatusCode::OK, &json!({ "queries": queries })),
                _ => error(StatusCode::INTERNAL_SERVER_ERROR, "could not save"),
            }
        }
        _ => method_not_allowed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_storage::sqlite::console::Column;

    #[tuff_core::watchdog]
    #[test]
    fn csv_quotes_and_defuses_formulas() {
        let out = QueryOutput {
            columns: vec![Column {
                name: "a".into(),
                decl_type: None,
            }],
            rows: vec![
                vec![Cell::Text("x,\"y\"".into())],
                vec![Cell::Text("=cmd()".into())],
                vec![Cell::Blob {
                    prefix: vec![0xab],
                    size: 9,
                }],
            ],
            has_more: false,
            truncated: false,
        };
        assert_eq!(
            String::from_utf8(to_csv(&out)).unwrap(),
            "a\r\n\"x,\"\"y\"\"\"\r\n'=cmd()\r\nblob:9:ab\r\n"
        );
    }
}
