//! The web panel's audit log: one JSON line per login, logout and
//! command, appended to the server file area by one actor that requests
//! never wait on.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use tuff_storage::config::ConfigStore;

pub const FILE: &str = "logs/admin-audit.jsonl";
/// Size at which the log rotates.
pub const ROTATE_BYTES: usize = 10 << 20;
/// Rotated files kept (`.1` newest).
pub const KEEP: usize = 5;
const QUEUE: usize = 1024;
const MAX_FIELD: usize = 4096;
pub const MAX_PAGE: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub ts: u64,
    pub principal: String,
    pub remote: String,
    pub action: String,
    pub target: String,
    pub detail: String,
    pub outcome: String,
}

impl Entry {
    pub fn new(principal: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            ts: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64),
            principal: principal.into(),
            remote: String::new(),
            action: action.into(),
            target: String::new(),
            detail: String::new(),
            outcome: "ok".into(),
        }
    }

    pub fn remote(mut self, remote: impl ToString) -> Self {
        self.remote = remote.to_string();
        self
    }

    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = target.into();
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    pub fn outcome(mut self, outcome: impl Into<String>) -> Self {
        self.outcome = outcome.into();
        self
    }

    pub fn to_json(&self) -> Value {
        let cap = |s: &str| -> String {
            if s.len() <= MAX_FIELD {
                return s.to_owned();
            }
            let mut end = MAX_FIELD;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}...", &s[..end])
        };
        json!({
            "ts": self.ts,
            "principal": cap(&self.principal),
            "remote": cap(&self.remote),
            "action": cap(&self.action),
            "target": cap(&redact(&self.target)),
            "detail": cap(&redact(&self.detail)),
            "outcome": cap(&self.outcome),
        })
    }
}

/// Masks values that look secret: login codes, and the value of a
/// `tuff config` key whose name mentions a secret.
pub fn redact(text: &str) -> String {
    redact_with(text, |_| false)
}

/// [`redact`], also masking the value given to any `tuff config` key
/// `secret` says is secret.
pub fn redact_with(text: &str, secret: impl Fn(&str) -> bool) -> String {
    let words: Vec<&str> = text.split(' ').collect();
    let command = words
        .iter()
        .position(|w| w.trim_start_matches('/') == "tuff")
        .filter(|&i| words.get(i + 1) == Some(&"config"));
    let mut out: Vec<String> = words.iter().map(|w| (*w).to_owned()).collect();
    if let Some(i) = command
        && let Some(key) = words.get(i + 2)
    {
        let lower = key.to_ascii_lowercase();
        if (secret(key)
            || ["secret", "password", "token", "key"]
                .iter()
                .any(|s| lower.contains(s)))
            && words.len() > i + 3
        {
            out.truncate(i + 3);
            out.push("<redacted>".into());
        }
    }
    for word in &mut out {
        if crate::auth::normalize_code(word).is_some() {
            *word = "<code>".into();
        }
    }
    out.join(" ")
}

enum Request {
    Record(Entry),
    Page {
        before: Option<u64>,
        limit: usize,
        filter: Filter,
        reply: oneshot::Sender<Vec<Value>>,
    },
    Flush(oneshot::Sender<()>),
}

/// The audit log's handle. Recording never blocks; a full queue or a
/// failed write is counted instead.
#[derive(Clone)]
pub struct AuditLog {
    tx: mpsc::Sender<Request>,
    failures: Arc<AtomicU64>,
    published: Option<mpsc::Sender<Value>>,
}

impl AuditLog {
    /// Starts the actor; `published` receives each entry as it is written.
    pub fn spawn(
        files: Option<Arc<dyn ConfigStore>>,
        published: Option<mpsc::Sender<Value>>,
    ) -> AuditLog {
        let (tx, rx) = mpsc::channel(QUEUE);
        let failures = Arc::new(AtomicU64::new(0));
        tuff_supervise::spawn("the audit log", restarting(files, rx, failures.clone()));
        AuditLog {
            tx,
            failures,
            published,
        }
    }

    pub fn detached() -> AuditLog {
        AuditLog {
            tx: mpsc::channel(1).0,
            failures: Arc::default(),
            published: None,
        }
    }

    pub fn record(&self, entry: Entry) {
        log::info!(
            target: "tuff::web::audit",
            "{} {} {} {} ({})",
            entry.principal,
            entry.action,
            redact(&entry.target),
            redact(&entry.detail),
            entry.outcome
        );
        if let Some(published) = &self.published {
            let _ = published.try_send(entry.to_json());
        }
        if self.tx.try_send(Request::Record(entry)).is_err() {
            self.failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Entries older than `before` (unix millis), newest first.
    pub async fn page(&self, before: Option<u64>, limit: usize) -> Vec<Value> {
        self.page_filtered(before, limit, Filter::default()).await
    }

    /// [`page`](Self::page), keeping only entries `filter` matches.
    pub async fn page_filtered(
        &self,
        before: Option<u64>,
        limit: usize,
        filter: Filter,
    ) -> Vec<Value> {
        let (reply, rx) = oneshot::channel();
        let request = Request::Page {
            filter,
            before,
            limit: limit.clamp(1, MAX_PAGE),
            reply,
        };
        if self.tx.send(request).await.is_err() {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    /// Writes and failures counted so far.
    pub fn failures(&self) -> u64 {
        self.failures.load(Ordering::Relaxed)
    }

    pub async fn flush(&self) {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Request::Flush(reply)).await.is_ok() {
            let _ = rx.await;
        }
    }
}

fn rotated(n: usize) -> String {
    format!("{FILE}.{n}")
}

async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> std::io::Result<T> {
    tokio::task::spawn_blocking(job)
        .await
        .map_err(std::io::Error::other)?
}

async fn restarting(
    files: Option<Arc<dyn ConfigStore>>,
    mut rx: mpsc::Receiver<Request>,
    failures: Arc<AtomicU64>,
) {
    use tuff_supervise::{CatchUnwind, count_panic, panic_message};
    while let Err(panic) = CatchUnwind::new(Box::pin(run(&files, &mut rx, &failures))).await {
        count_panic();
        failures.fetch_add(1, Ordering::Relaxed);
        log::error!(
            "the audit log panicked: {}; restarting it from its file",
            panic_message(&*panic)
        );
    }
}

async fn run(
    files: &Option<Arc<dyn ConfigStore>>,
    rx: &mut mpsc::Receiver<Request>,
    failures: &Arc<AtomicU64>,
) {
    let mut size = match &files {
        Some(f) => {
            let f = f.clone();
            blocking(move || f.read_file(FILE, ROTATE_BYTES * 2))
                .await
                .ok()
                .flatten()
                .map_or(0, |b| b.len())
        }
        None => 0,
    };
    while let Some(request) = rx.recv().await {
        match request {
            Request::Record(entry) => {
                let Some(files) = files.clone() else {
                    failures.fetch_add(1, Ordering::Relaxed);
                    continue;
                };
                let mut line = entry.to_json().to_string();
                line.push('\n');
                let rotate = size > 0 && size + line.len() > ROTATE_BYTES;
                let len = line.len();
                let written = blocking(move || {
                    if rotate {
                        rotate_files(&*files)?;
                    }
                    files.append_file(FILE, line.as_bytes())
                })
                .await;
                match written {
                    Ok(()) => size = if rotate { len } else { size + len },
                    Err(err) => {
                        failures.fetch_add(1, Ordering::Relaxed);
                        log::warn!("cannot write the web audit log: {err}");
                    }
                }
            }
            Request::Page {
                before,
                limit,
                filter,
                reply,
            } => {
                let Some(files) = files.clone() else {
                    let _ = reply.send(Vec::new());
                    continue;
                };
                let page = blocking(move || Ok(read_page(&*files, before, limit, &filter)))
                    .await
                    .unwrap_or_default();
                let _ = reply.send(page);
            }
            Request::Flush(reply) => {
                let _ = reply.send(());
            }
        }
    }
}

fn rotate_files(files: &dyn ConfigStore) -> std::io::Result<()> {
    files.remove_file(&rotated(KEEP))?;
    for n in (1..KEEP).rev() {
        if let Some(bytes) = files.read_file(&rotated(n), ROTATE_BYTES * 2)? {
            files.write_file(&rotated(n + 1), &bytes)?;
            files.remove_file(&rotated(n))?;
        }
    }
    if let Some(bytes) = files.read_file(FILE, ROTATE_BYTES * 2)? {
        files.write_file(&rotated(1), &bytes)?;
        files.remove_file(FILE)?;
    }
    Ok(())
}

/// Which entries a page keeps: a principal (any case) and an action or
/// action prefix ending in `.` (`config.`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    pub principal: Option<String>,
    pub action: Option<String>,
}

impl Filter {
    pub fn matches(&self, entry: &Value) -> bool {
        let field = |name: &str| entry.get(name).and_then(Value::as_str).unwrap_or_default();
        self.principal
            .as_deref()
            .is_none_or(|p| field("principal").eq_ignore_ascii_case(p))
            && self.action.as_deref().is_none_or(|a| {
                let action = field("action");
                action == a || (a.ends_with('.') && action.starts_with(a))
            })
    }
}

fn read_page(
    files: &dyn ConfigStore,
    before: Option<u64>,
    limit: usize,
    filter: &Filter,
) -> Vec<Value> {
    let mut out = Vec::new();
    let names = std::iter::once(FILE.to_owned()).chain((1..=KEEP).map(rotated));
    for name in names {
        let Ok(Some(bytes)) = files.read_file(&name, ROTATE_BYTES * 2) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        for line in text.lines().rev() {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let ts = value.get("ts").and_then(Value::as_u64).unwrap_or(0);
            if before.is_some_and(|b| ts >= b) || !filter.matches(&value) {
                continue;
            }
            out.push(value);
            if out.len() >= limit {
                return out;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &tuff_storage::test_dir::TestDir) -> Arc<dyn ConfigStore> {
        Arc::new(tuff_storage::vanilla::config::VanillaConfig::new(
            dir.path(),
        ))
    }

    #[tuff_core::watchdog]
    #[test]
    fn secrets_are_redacted() {
        assert_eq!(
            redact("tuff config my.apiToken hunter2 more"),
            "tuff config my.apiToken <redacted>"
        );
        assert_eq!(
            redact("/tuff config tuff.http.port 8080"),
            "/tuff config tuff.http.port 8080"
        );
        assert_eq!(redact("code 1234-5678-9012-3456"), "code <code>");
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn entries_are_written_paged_and_published() {
        let dir = tuff_storage::test_dir::TestDir::new();
        let (tx, mut published) = mpsc::channel(8);
        let audit = AuditLog::spawn(Some(store(&dir)), Some(tx));
        for (i, action) in ["login", "command", "logout"].into_iter().enumerate() {
            let mut entry = Entry::new("Alex", action)
                .remote("127.0.0.1")
                .target("say hi");
            entry.ts = 1000 + i as u64;
            audit.record(entry);
        }
        audit.record(Entry::new("Alex", "command").detail("tuff config a.secret x"));
        audit.flush().await;
        assert_eq!(published.recv().await.unwrap()["action"], "login");
        let page = audit.page(None, 10).await;
        assert_eq!(page.len(), 4);
        assert_eq!(page[0]["detail"], "tuff config a.secret <redacted>");
        let older = audit.page(Some(1002), 10).await;
        let actions: Vec<_> = older.iter().map(|v| v["action"].clone()).collect();
        assert_eq!(actions, ["command", "login"]);
        assert_eq!(audit.page(Some(1002), 1).await.len(), 1);
        let only = |principal: Option<&str>, action: Option<&str>| Filter {
            principal: principal.map(str::to_owned),
            action: action.map(str::to_owned),
        };
        let commands = audit
            .page_filtered(None, 10, only(Some("alex"), Some("command")))
            .await;
        assert_eq!(commands.len(), 2);
        assert!(
            audit
                .page_filtered(None, 10, only(Some("Steve"), None))
                .await
                .is_empty()
        );
        assert!(only(None, Some("config.")).matches(&json!({"action": "config.set"})));
        assert!(!only(None, Some("config")).matches(&json!({"action": "config.set"})));
        assert_eq!(audit.failures(), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn rotation_keeps_five_files() {
        let dir = tuff_storage::test_dir::TestDir::new();
        let files = store(&dir);
        for n in 0..7 {
            files
                .append_file(FILE, format!("{n}\n").as_bytes())
                .unwrap();
            rotate_files(&*files).unwrap();
        }
        assert_eq!(files.read_file(FILE, 100).unwrap(), None);
        assert_eq!(files.read_file(&rotated(1), 100).unwrap().unwrap(), b"6\n");
        assert_eq!(files.read_file(&rotated(5), 100).unwrap().unwrap(), b"2\n");
        assert_eq!(files.read_file(&rotated(6), 100).unwrap(), None);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn failures_are_counted() {
        let audit = AuditLog::spawn(None, None);
        audit.record(Entry::new("x", "login"));
        audit.flush().await;
        assert_eq!(audit.failures(), 1);
    }
}
