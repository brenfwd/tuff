//! The Backups page's API over the backup jobs actor:
//!
//! - `GET /backups`: the backups, the running and last job, the staged
//!   restore and the upload limit (`backups.read`).
//! - `POST /backups/create`, `POST /backups/cancel` (`backups.manage`).
//! - `GET /backups/<name>`: the file, resumable with ranges
//!   (`backups.read`); only names the listing gives resolve.
//! - `DELETE /backups/<name>` (`backups.manage`).
//! - `PUT /backups/upload?filename=<shown name>`: the raw file with a
//!   `Content-Length`, streamed to disk, checked completely and listed
//!   under a new name (`backups.manage`).
//! - `POST /backups/restore` with `{"name", "confirm"}` (the level name):
//!   stages the backup next to the world for the next start;
//!   `DELETE /backups/restore` drops it; `POST /backups/restore/shutdown`
//!   stops the server so the restore applies (`backups.restore`).
//!
//! The route streams request bodies, so it has no request timeout: a
//! large upload is checked before the response.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use hyper::{Method, StatusCode};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use super::api::{method_not_allowed, require};
use super::audit::Entry;
use super::auth::{AuthContext, Scope};
use super::guard::{self, error};
use super::host::{HostError, HostFuture, PanelHost};
use tuff_http::{Request, Response};
use tuff_storage::backup::restore::{self, Checked, Kind, RestoreError, Staged};
use tuff_storage::backup::space;

/// The event topic backup jobs and restores report on.
pub const TOPIC: &str = "jobs";
pub const MAX_UPLOAD_KEY: &str = "tuff.web.maxBackupUpload";
pub const DEFAULT_MAX_UPLOAD_MIB: i64 = 16 * 1024;
/// How many times the upload limit a backup may unpack to.
pub const UNPACKED_FACTOR: u64 = 4;
const MAX_JSON: usize = 64 * 1024;
const UPLOADS_PER_PRINCIPAL: usize = 1;
const UPLOADS_TOTAL: usize = 2;
/// Upload chunks queued for the disk writer; the bound is the
/// back-pressure on the socket.
const WRITE_QUEUE: usize = 8;
const PROGRESS_EVERY: Duration = Duration::from_millis(500);
const MAX_SHOWN_NAME: usize = 128;

/// The upload limit in bytes.
pub fn max_upload(host: &dyn PanelHost) -> u64 {
    let mib = host
        .config_int(MAX_UPLOAD_KEY)
        .unwrap_or(DEFAULT_MAX_UPLOAD_MIB)
        .max(1) as u64;
    mib.saturating_mul(1 << 20)
}

/// The most a backup may unpack to when checked or restored.
pub fn max_unpacked(host: &dyn PanelHost) -> u64 {
    max_upload(host).saturating_mul(UNPACKED_FACTOR)
}

/// Why a transfer may not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Busy {
    /// This principal is uploading already.
    Yours,
    /// The server-wide upload limit is reached.
    Full,
    /// A restore is being staged.
    Restoring,
}

enum Message {
    Begin {
        who: String,
        restore: bool,
        reply: oneshot::Sender<Result<u64, Busy>>,
    },
    End(u64),
}

/// Who is uploading or staging a restore: at most one upload per
/// principal, two in all, and one restore.
#[derive(Clone)]
pub struct Transfers {
    // Unbounded: a ticket's drop must never lose its `End`, and `Begin`s
    // are bounded by the listener's connection cap.
    tx: mpsc::UnboundedSender<Message>,
}

/// Holds a transfer slot until dropped.
pub struct Ticket {
    id: u64,
    tx: mpsc::UnboundedSender<Message>,
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let _ = self.tx.send(Message::End(self.id));
    }
}

impl Transfers {
    pub fn spawn() -> Transfers {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut active: HashMap<u64, (String, bool)> = HashMap::new();
            let mut next = 0u64;
            while let Some(message) = rx.recv().await {
                match message {
                    Message::Begin {
                        who,
                        restore,
                        reply,
                    } => {
                        let busy = if restore {
                            active.values().any(|(_, r)| *r).then_some(Busy::Restoring)
                        } else {
                            let uploads = active.values().filter(|(_, r)| !*r);
                            let (mine, all) = uploads.fold((0, 0), |(mine, all), (w, _)| {
                                (mine + usize::from(*w == who), all + 1)
                            });
                            if mine >= UPLOADS_PER_PRINCIPAL {
                                Some(Busy::Yours)
                            } else if all >= UPLOADS_TOTAL {
                                Some(Busy::Full)
                            } else {
                                None
                            }
                        };
                        let answer = match busy {
                            Some(busy) => Err(busy),
                            None => {
                                next += 1;
                                active.insert(next, (who, restore));
                                Ok(next)
                            }
                        };
                        if let (Err(_), Ok(id)) = (reply.send(answer), answer) {
                            active.remove(&id);
                        }
                    }
                    Message::End(id) => {
                        active.remove(&id);
                    }
                }
            }
        });
        Transfers { tx }
    }

    /// A slot for an upload by `who`, or for staging a restore.
    pub async fn begin(&self, who: &str, restore: bool) -> Result<Ticket, Busy> {
        let (reply, answer) = oneshot::channel();
        let _ = self.tx.send(Message::Begin {
            who: who.to_owned(),
            restore,
            reply,
        });
        let id = answer.await.map_err(|_| Busy::Full)??;
        Ok(Ticket {
            id,
            tx: self.tx.clone(),
        })
    }
}

/// What the Backups page needs from the server.
pub trait BackupHost: Send + Sync {
    /// The backups, newest first, as the page lists them.
    fn list(&self) -> HostFuture<'_, Result<Value, HostError>>;
    /// The running and the last job.
    fn status(&self) -> HostFuture<'_, Value>;
    /// The world's level name.
    fn level(&self) -> String;
    /// The world's kind and path, when a backup can be restored into it.
    fn world_target(&self) -> Option<(Kind, PathBuf)>;
    /// Starts a backup, answering its name and the running job.
    fn create(
        &self,
        by: String,
        include_caches: bool,
    ) -> HostFuture<'_, Result<(String, Value), HostError>>;
    /// Cancels the running backup, answering its name.
    fn cancel(&self) -> HostFuture<'_, Result<String, HostError>>;
    fn delete<'a>(&'a self, name: &'a str) -> HostFuture<'a, Result<(), HostError>>;
    /// A listed backup's file.
    fn path<'a>(&'a self, name: &'a str) -> HostFuture<'a, Result<PathBuf, HostError>>;
    /// Where backups are kept.
    fn directory(&self) -> HostFuture<'_, Result<PathBuf, HostError>>;
    /// Stops the server; `false` when it is stopping already.
    fn stop(&self) -> HostFuture<'_, bool>;
}

fn failure(err: &HostError) -> Response {
    error(err.status, &err.message)
}

fn restore_failure(err: &RestoreError) -> Response {
    let status = match err {
        RestoreError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
        RestoreError::Mismatch(_) => StatusCode::CONFLICT,
        RestoreError::NoSpace { .. } => StatusCode::INSUFFICIENT_STORAGE,
        RestoreError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    error(status, &err.to_string())
}

pub(super) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

pub(super) async fn read_json(req: &mut Request) -> Result<Value, Response> {
    let typed = req
        .header("content-type")
        .is_some_and(|t| t.trim_start().starts_with("application/json"));
    if !typed {
        return Err(error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "expected application/json",
        ));
    }
    let Some(mut body) = req.take_body_stream() else {
        return serde_json::from_slice(&req.body)
            .map_err(|_| error(StatusCode::BAD_REQUEST, "bad json"));
    };
    let mut bytes = Vec::new();
    loop {
        match body.chunk().await {
            Ok(Some(chunk)) => {
                if bytes.len() + chunk.len() > MAX_JSON {
                    return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "body too large"));
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(err) => return Err(err.response()),
        }
    }
    serde_json::from_slice(&bytes).map_err(|_| error(StatusCode::BAD_REQUEST, "bad json"))
}

pub async fn handle(host: Arc<dyn PanelHost>, mut req: Request, transfers: Transfers) -> Response {
    let ctx = match require(&req, Some(Scope::BackupsRead)) {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    let rest = req.rest.trim_end_matches('/').to_owned();
    let method = req.method.clone();
    let scope = match (rest.as_str(), &method) {
        ("", &Method::GET) => Scope::BackupsRead,
        ("/create" | "/cancel", &Method::POST) => Scope::BackupsManage,
        ("/upload", &Method::PUT | &Method::POST) => Scope::BackupsManage,
        ("/restore", &Method::POST | &Method::DELETE) => Scope::BackupsRestore,
        ("/restore/shutdown", &Method::POST) => Scope::BackupsRestore,
        ("" | "/create" | "/cancel" | "/upload" | "/restore" | "/restore/shutdown", _) => {
            return method_not_allowed();
        }
        (path, method) => match path.strip_prefix('/') {
            Some(name) if !name.contains('/') => match *method {
                Method::GET | Method::HEAD => Scope::BackupsRead,
                Method::DELETE => Scope::BackupsManage,
                _ => return method_not_allowed(),
            },
            _ => return error(StatusCode::NOT_FOUND, "no such endpoint"),
        },
    };
    if let Err(resp) = ctx.require(scope) {
        return resp;
    }
    match (rest.as_str(), &method) {
        ("", _) => list(&*host).await,
        ("/create", _) => create(&*host, &mut req, &ctx).await,
        ("/cancel", _) => cancel(&*host, &req, &ctx).await,
        ("/upload", _) => upload(&*host, req, &ctx, &transfers).await,
        ("/restore", &Method::POST) => stage(&*host, req, &ctx, &transfers).await,
        ("/restore", _) => unstage(&*host, &req, &ctx).await,
        ("/restore/shutdown", _) => shutdown(&*host, &req, &ctx).await,
        (path, &Method::DELETE) => delete(&*host, &req, &ctx, &path[1..]).await,
        (path, _) => download(&*host, &req, &ctx, &path[1..]).await,
    }
}

fn audit(host: &dyn PanelHost, req: &Request, ctx: &AuthContext, action: &str) -> Entry {
    Entry::new(ctx.principal.name(), action).remote(guard::client_ip(host, req))
}

async fn list(host: &dyn PanelHost) -> Response {
    let backups = match host.backups().list().await {
        Ok(list) => list,
        Err(err) => return failure(&err),
    };
    let status = host.backups().status().await;
    let target = host.backups().world_target();
    let staged = match &target {
        Some((_, world)) => {
            let world = world.clone();
            blocking(move || restore::staged(&world)).await.flatten()
        }
        None => None,
    };
    Response::json(
        StatusCode::OK,
        &json!({
            "level": host.backups().level(),
            "world": target.map(|(kind, _)| kind),
            "backups": backups,
            "status": status,
            "staged": staged,
            "maxUpload": max_upload(host),
        }),
    )
}

async fn create(host: &dyn PanelHost, req: &mut Request, ctx: &AuthContext) -> Response {
    let entry = audit(host, req, ctx, "backup.create");
    let include_caches = match read_json(req).await {
        Ok(body) => body.get("includeCaches").and_then(Value::as_bool) == Some(true),
        Err(_) => false,
    };
    match host
        .backups()
        .create(ctx.principal.name(), include_caches)
        .await
    {
        Ok((name, running)) => {
            host.web().audit.record(entry.target(name));
            Response::json(StatusCode::OK, &json!({ "running": running }))
        }
        Err(err) => {
            host.web().audit.record(entry.outcome(err.message.clone()));
            failure(&err)
        }
    }
}

async fn cancel(host: &dyn PanelHost, req: &Request, ctx: &AuthContext) -> Response {
    match host.backups().cancel().await {
        Ok(name) => {
            host.web()
                .audit
                .record(audit(host, req, ctx, "backup.cancel").target(name.clone()));
            Response::json(StatusCode::OK, &json!({ "name": name }))
        }
        Err(err) => failure(&err),
    }
}

async fn delete(host: &dyn PanelHost, req: &Request, ctx: &AuthContext, name: &str) -> Response {
    let entry = audit(host, req, ctx, "backup.delete").target(name);
    match host.backups().delete(name).await {
        Ok(()) => {
            host.web().audit.record(entry);
            Response::json(StatusCode::OK, &json!({ "deleted": name }))
        }
        Err(err) => {
            if !err.quiet {
                host.web().audit.record(entry.outcome(err.message.clone()));
            }
            failure(&err)
        }
    }
}

fn content_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, ext)| ext) {
        Some("zip") => "application/zip",
        Some("mcserver") => "application/vnd.sqlite3",
        _ => "application/octet-stream",
    }
}

async fn download(host: &dyn PanelHost, req: &Request, ctx: &AuthContext, name: &str) -> Response {
    let path = match host.backups().path(name).await {
        Ok(path) => path,
        Err(err) => return failure(&err),
    };
    let mut resp = Response::file(req, content_type(name), path, name).await;
    resp.set_header(
        "content-disposition",
        &format!("attachment; filename=\"{name}\""),
    );
    if req.method == Method::GET && req.header("range").is_none() && resp.status == StatusCode::OK {
        host.web()
            .audit
            .record(audit(host, req, ctx, "backup.download").target(name));
    }
    resp
}

fn shown_name(req: &Request) -> String {
    req.query_param("filename")
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_SHOWN_NAME)
        .collect()
}

async fn upload(
    host: &dyn PanelHost,
    mut req: Request,
    ctx: &AuthContext,
    transfers: &Transfers,
) -> Response {
    let shown = shown_name(&req);
    let entry = audit(host, &req, ctx, "backup.upload").detail(shown.clone());
    let Some(len) = req
        .header("content-length")
        .and_then(|v| v.trim().parse::<u64>().ok())
    else {
        return error(
            StatusCode::LENGTH_REQUIRED,
            "send the file with a Content-Length",
        );
    };
    if len == 0 {
        return error(StatusCode::BAD_REQUEST, "the file is empty");
    }
    let cap = max_upload(host);
    if len > cap {
        host.web()
            .audit
            .record(entry.outcome(format!("{len} bytes, over the limit")));
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            &format!(
                "backups over {} MiB cannot be uploaded ({MAX_UPLOAD_KEY})",
                cap >> 20
            ),
        );
    }
    let _ticket = match transfers.begin(&ctx.principal.name(), false).await {
        Ok(ticket) => ticket,
        Err(Busy::Yours) => {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "you are uploading a backup already",
            );
        }
        Err(_) => {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "too many backups are being uploaded; try again later",
            );
        }
    };
    let dir = match host.backups().directory().await {
        Ok(dir) => dir,
        Err(err) => return failure(&err),
    };
    let Some(body) = req.take_body_stream() else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "no request body");
    };
    let temp = restore::upload_path(&dir);
    let max_unpacked = max_unpacked(host);
    let result = receive(body, len, &dir, &temp, max_unpacked).await;
    let checked = match result {
        Ok(checked) => checked,
        Err(resp) => {
            let temp = temp.clone();
            blocking(move || std::fs::remove_file(temp)).await;
            host.web().audit.record(entry.outcome(format!(
                "refused: {}",
                resp.body
                    .bytes()
                    .and_then(|b| serde_json::from_slice::<Value>(b).ok())
                    .and_then(|v| v["error"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| resp.status.to_string())
            )));
            return resp;
        }
    };
    let level = host.backups().level();
    let kind = checked.kind;
    let published = {
        let (dir, temp) = (dir.clone(), temp.clone());
        blocking(move || restore::publish_upload(&dir, &level, &temp, kind)).await
    };
    match published {
        Some(Ok(info)) => {
            host.web().audit.record(
                entry
                    .target(info.name.clone())
                    .detail(format!("{shown} ({len} bytes)")),
            );
            Response::json(
                StatusCode::OK,
                &json!({ "backup": info, "checked": checked }),
            )
        }
        other => {
            blocking(move || std::fs::remove_file(temp)).await;
            let why = match other {
                Some(Err(err)) => err.to_string(),
                _ => "the upload could not be listed".to_owned(),
            };
            host.web().audit.record(entry.outcome(why.clone()));
            error(StatusCode::INTERNAL_SERVER_ERROR, &why)
        }
    }
}

/// Streams `len` bytes of `body` into the new file `temp` in `dir` and
/// checks it as a backup.
async fn receive(
    mut body: tuff_http::BodyStream,
    len: u64,
    dir: &Path,
    temp: &Path,
    max_unpacked: u64,
) -> Result<Checked, Response> {
    let opened = {
        let (dir, temp) = (dir.to_path_buf(), temp.to_path_buf());
        blocking(move || -> Result<std::fs::File, Response> {
            std::fs::create_dir_all(&dir)
                .map_err(|err| error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()))?;
            let needed = len.saturating_mul(2);
            if let Some(free) = space::available(&dir)
                && free < needed
            {
                return Err(error(
                    StatusCode::INSUFFICIENT_STORAGE,
                    &format!("not enough free disk space: {needed} bytes needed, {free} free"),
                ));
            }
            std::fs::File::create_new(&temp)
                .map_err(|err| error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()))
        })
        .await
        .unwrap_or_else(|| Err(error(StatusCode::INTERNAL_SERVER_ERROR, "upload failed")))?
    };
    let (chunks, mut queue) = mpsc::channel::<Bytes>(WRITE_QUEUE);
    let writer = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        use std::io::Write;
        let mut file = opened;
        while let Some(chunk) = queue.blocking_recv() {
            file.write_all(&chunk)?;
        }
        file.sync_all()
    });
    let mut received = 0u64;
    let streamed = loop {
        match body.chunk().await {
            Ok(Some(chunk)) => {
                received += chunk.len() as u64;
                if received > len {
                    break Err(error(
                        StatusCode::BAD_REQUEST,
                        "the body is longer than its Content-Length",
                    ));
                }
                if chunks.send(chunk).await.is_err() {
                    break Ok(());
                }
            }
            Ok(None) => break Ok(()),
            Err(err) => break Err(error(err.response().status, &err.to_string())),
        }
    };
    drop(chunks);
    let written = writer.await;
    streamed?;
    match written {
        Ok(Ok(())) => {}
        Ok(Err(err)) => return Err(error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string())),
        Err(_) => return Err(error(StatusCode::INTERNAL_SERVER_ERROR, "upload failed")),
    }
    if received != len {
        return Err(error(StatusCode::BAD_REQUEST, "the upload ended early"));
    }
    let temp = temp.to_path_buf();
    match blocking(move || restore::inspect(&temp, max_unpacked)).await {
        Some(Ok(checked)) => Ok(checked),
        Some(Err(err)) => Err(restore_failure(&err)),
        None => Err(error(StatusCode::INTERNAL_SERVER_ERROR, "the check failed")),
    }
}

async fn stage(
    host: &dyn PanelHost,
    mut req: Request,
    ctx: &AuthContext,
    transfers: &Transfers,
) -> Response {
    let body = match read_json(&mut req).await {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let name = body["name"].as_str().unwrap_or_default().to_owned();
    let level = host.backups().level();
    if body["confirm"].as_str() != Some(level.as_str()) {
        return error(
            StatusCode::BAD_REQUEST,
            &format!("type the level name {level} to confirm"),
        );
    }
    let Some((kind, world)) = host.backups().world_target() else {
        return error(
            StatusCode::CONFLICT,
            "this world's storage cannot be restored",
        );
    };
    let path = match host.backups().path(&name).await {
        Ok(path) => path,
        Err(err) => return failure(&err),
    };
    let _ticket = match transfers.begin(&ctx.principal.name(), true).await {
        Ok(ticket) => ticket,
        Err(_) => return error(StatusCode::CONFLICT, "a restore is being staged already"),
    };
    let entry = audit(host, &req, ctx, "backup.restore").target(name.clone());
    let events = host.web().events.clone();
    let max_unpacked = max_unpacked(host);
    let by = ctx.principal.name();
    let staged = {
        let name = name.clone();
        blocking(move || {
            let mut last = Instant::now();
            restore::stage(
                &path,
                &name.clone(),
                &world,
                kind,
                max_unpacked,
                &by,
                |done, total| {
                    if last.elapsed() >= PROGRESS_EVERY {
                        last = Instant::now();
                        let data = json!({
                            "kind": "restore",
                            "name": name,
                            "progress": { "bytes_done": done, "bytes_total": total },
                        });
                        events.publish(TOPIC, "progress", data.to_string());
                    }
                    Ok(())
                },
            )
        })
        .await
    };
    match staged {
        Some(Ok(staged)) => {
            host.web().audit.record(entry);
            publish_staged(host, Some(&staged));
            Response::json(StatusCode::OK, &json!({ "staged": staged }))
        }
        Some(Err(err)) => {
            host.web().audit.record(entry.outcome(err.to_string()));
            restore_failure(&err)
        }
        None => error(StatusCode::INTERNAL_SERVER_ERROR, "staging failed"),
    }
}

fn publish_staged(host: &dyn PanelHost, staged: Option<&Staged>) {
    let data = json!({ "kind": "restore", "staged": staged });
    host.web().events.publish(TOPIC, "staged", data.to_string());
}

async fn unstage(host: &dyn PanelHost, req: &Request, ctx: &AuthContext) -> Response {
    let Some((_, world)) = host.backups().world_target() else {
        return error(StatusCode::CONFLICT, "no restore is staged");
    };
    let found = {
        let world = world.clone();
        blocking(move || restore::staged(&world)).await.flatten()
    };
    let Some(found) = found else {
        return error(StatusCode::CONFLICT, "no restore is staged");
    };
    match blocking(move || restore::unstage(&world)).await {
        Some(Ok(())) => {
            host.web().audit.record(
                audit(host, req, ctx, "backup.restore.cancel").target(found.backup.clone()),
            );
            publish_staged(host, None);
            Response::json(StatusCode::OK, &json!({ "cancelled": found.backup }))
        }
        Some(Err(err)) => error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
        None => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "could not drop the restore",
        ),
    }
}

async fn shutdown(host: &dyn PanelHost, req: &Request, ctx: &AuthContext) -> Response {
    let staged = match host.backups().world_target() {
        Some((_, world)) => blocking(move || restore::staged(&world)).await.flatten(),
        None => None,
    };
    let Some(staged) = staged else {
        return error(StatusCode::CONFLICT, "no restore is staged");
    };
    host.web()
        .audit
        .record(audit(host, req, ctx, "backup.restore.shutdown").target(staged.backup.clone()));
    log::info!(
        "{} stopped the server to restore backup {}",
        ctx.principal.name(),
        staged.backup
    );
    if !host.backups().stop().await {
        return error(StatusCode::SERVICE_UNAVAILABLE, "the server is stopping");
    }
    Response::json(StatusCode::ACCEPTED, &json!({ "stopping": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn transfers_limit_uploads_per_principal_and_in_all() {
        let transfers = Transfers::spawn();
        let alex = transfers.begin("alex", false).await.unwrap();
        assert_eq!(
            transfers.begin("alex", false).await.err(),
            Some(Busy::Yours)
        );
        let sam = transfers.begin("sam", false).await.unwrap();
        assert_eq!(transfers.begin("kim", false).await.err(), Some(Busy::Full));
        let restore = transfers.begin("alex", true).await.unwrap();
        assert_eq!(
            transfers.begin("kim", true).await.err(),
            Some(Busy::Restoring)
        );
        drop(alex);
        let again = transfers.begin("alex", false).await.unwrap();
        drop((again, sam, restore));
        transfers.begin("kim", true).await.unwrap();
    }

    #[tuff_core::watchdog]
    #[test]
    fn downloads_name_their_type() {
        assert_eq!(content_type("world-20261003-123456.zip"), "application/zip");
        assert_eq!(
            content_type("world-20261003-123456.mcserver"),
            "application/vnd.sqlite3"
        );
        assert_eq!(content_type("x"), "application/octet-stream");
    }
}
