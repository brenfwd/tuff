//! The Data packs page's API over the world's pack store and the data
//! pack actor:
//!
//! - `GET /datapacks`: every pack with its metadata, whether and where it
//!   is enabled, and why a stored pack cannot be read (`datapacks.read`).
//! - `GET /datapacks/<name>`: the pack as a zip; a folder pack is zipped
//!   on the fly (`datapacks.read`).
//! - `PUT /datapacks/upload?name=<pack name>&filename=<file>&replace=1&enable=1`:
//!   a zip with a `Content-Length`, checked by the pack store and stored in
//!   the world (`datapacks.manage`).
//! - `POST /datapacks/enable`, `POST /datapacks/disable` with `{"id"}`,
//!   and `POST /datapacks/reload` change what is loaded the way
//!   `/datapack enable|disable` and `/reload` do (`datapacks.manage`).
//! - `DELETE /datapacks/<name>`: disables the pack, then deletes it
//!   (`datapacks.manage`).
//!
//! Pack store calls run on the blocking pool against the live (gated)
//! backend; enabling and reloading go through [`DataPacks`].

use std::sync::Arc;

use hyper::{Method, StatusCode};
use serde_json::{Value, json};

use super::api::{method_not_allowed, require};
use super::audit::Entry;
use super::auth::{AuthContext, Scope};
use super::backups::{Busy, Transfers, blocking, read_json};
use super::guard::{self, error};
use super::host::{HostError, HostFuture, PanelHost};
use tuff_http::{Request, Response};

/// What the Data packs page needs from the server.
pub trait DatapackHost: Send + Sync {
    /// Every pack, what is enabled, and stored packs that cannot be read.
    fn listing(&self) -> HostFuture<'_, Result<Value, HostError>>;
    /// Enables pack `id` above every other; answers the new selection.
    fn enable<'a>(&'a self, id: &'a str) -> HostFuture<'a, Result<Value, HostError>>;
    fn disable<'a>(&'a self, id: &'a str) -> HostFuture<'a, Result<Value, HostError>>;
    /// Reloads the selection plus every new pack not disabled.
    fn reload(&self) -> HostFuture<'_, Result<Value, HostError>>;
    /// Disables and deletes a stored pack, answering its id.
    fn remove<'a>(&'a self, name: &'a str) -> HostFuture<'a, Result<String, HostError>>;
    /// A stored pack's file name and zip bytes.
    fn archive<'a>(&'a self, name: &'a str)
    -> HostFuture<'a, Result<(String, Vec<u8>), HostError>>;
    /// Stores a zip as pack `name`, answering its id and the page's row.
    fn add<'a>(
        &'a self,
        name: &'a str,
        bytes: Vec<u8>,
        replace: bool,
        enable: bool,
    ) -> HostFuture<'a, Result<(String, Value), HostError>>;
    /// The pack name an uploaded file name suggests.
    fn suggested_name(&self, file: &str) -> Option<String>;
    fn max_upload(&self) -> u64;
}

fn failure(err: &HostError) -> Response {
    error(err.status, &err.message)
}

pub async fn handle(host: Arc<dyn PanelHost>, req: Request, transfers: Transfers) -> Response {
    let ctx = match require(&req, Some(Scope::DatapacksRead)) {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    let rest = req.rest.trim_end_matches('/').to_owned();
    let method = req.method.clone();
    let scope = match (rest.as_str(), &method) {
        ("", &Method::GET) => Scope::DatapacksRead,
        ("/upload", &Method::PUT | &Method::POST) => Scope::DatapacksManage,
        ("/enable" | "/disable" | "/reload", &Method::POST) => Scope::DatapacksManage,
        ("" | "/upload" | "/enable" | "/disable" | "/reload", _) => return method_not_allowed(),
        (path, method) => match path.strip_prefix('/') {
            Some(name) if !name.contains('/') => match *method {
                Method::GET | Method::HEAD => Scope::DatapacksRead,
                Method::DELETE => Scope::DatapacksManage,
                _ => return method_not_allowed(),
            },
            _ => return error(StatusCode::NOT_FOUND, "no such endpoint"),
        },
    };
    if let Err(resp) = ctx.require(scope) {
        return resp;
    }
    let host = &*host;
    match (rest.as_str(), &method) {
        ("", _) => match host.datapacks().listing().await {
            Ok(listing) => Response::json(StatusCode::OK, &listing),
            Err(err) => failure(&err),
        },
        ("/upload", _) => upload_pack(host, req, &ctx, &transfers).await,
        ("/enable", _) => change(host, req, &ctx, true).await,
        ("/disable", _) => change(host, req, &ctx, false).await,
        ("/reload", _) => reload_all(host, &req, &ctx).await,
        (path, &Method::DELETE) => delete(host, &req, &ctx, &path[1..]).await,
        (path, _) => download(host, &req, &ctx, &path[1..]).await,
    }
}

fn audit(host: &dyn PanelHost, req: &Request, ctx: &AuthContext, action: &str) -> Entry {
    Entry::new(ctx.principal.name(), action).remote(guard::client_ip(host, req))
}

/// Answers `result`, recording `entry` (with the refusal as its outcome
/// unless the refusal is quiet).
fn answer(host: &dyn PanelHost, entry: Entry, result: Result<Value, HostError>) -> Response {
    match result {
        Ok(body) => {
            host.web().audit.record(entry);
            Response::json(StatusCode::OK, &body)
        }
        Err(err) => {
            if !err.quiet {
                host.web().audit.record(entry.outcome(err.message.clone()));
            }
            failure(&err)
        }
    }
}

async fn change(host: &dyn PanelHost, mut req: Request, ctx: &AuthContext, on: bool) -> Response {
    let body = match read_json(&mut req).await {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let id = body["id"].as_str().unwrap_or_default().to_owned();
    let action = if on {
        "datapack.enable"
    } else {
        "datapack.disable"
    };
    let entry = audit(host, &req, ctx, action).target(id.clone());
    let packs = host.datapacks();
    let result = if on {
        packs.enable(&id).await
    } else {
        packs.disable(&id).await
    };
    answer(host, entry, result)
}

async fn reload_all(host: &dyn PanelHost, req: &Request, ctx: &AuthContext) -> Response {
    let entry = audit(host, req, ctx, "datapack.reload");
    let result = host.datapacks().reload().await.map_err(HostError::loud);
    answer(host, entry, result)
}

async fn delete(host: &dyn PanelHost, req: &Request, ctx: &AuthContext, name: &str) -> Response {
    let entry = audit(host, req, ctx, "datapack.remove").target(name);
    match host.datapacks().remove(name).await {
        Ok(id) => answer(host, entry.target(id.clone()), Ok(json!({ "removed": id }))),
        Err(err) => answer(host, entry, Err(err)),
    }
}

async fn download(host: &dyn PanelHost, req: &Request, ctx: &AuthContext, name: &str) -> Response {
    let (file, bytes) = match host.datapacks().archive(name).await {
        Ok(found) => found,
        Err(err) => return failure(&err),
    };
    let mut resp = Response::conditional(req, "application/zip", bytes.into());
    crate::upload::attachment(&mut resp, &file);
    if req.method == Method::GET && req.header("range").is_none() && resp.status == StatusCode::OK {
        host.web()
            .audit
            .record(audit(host, req, ctx, "datapack.download").target(format!("file/{name}")));
    }
    resp
}

async fn upload_pack(
    host: &dyn PanelHost,
    mut req: Request,
    ctx: &AuthContext,
    transfers: &Transfers,
) -> Response {
    let packs = host.datapacks();
    let shown = crate::upload::shown_name(&req);
    let entry = audit(host, &req, ctx, "datapack.upload").detail(shown.clone());
    let refuse = |resp: Response| {
        host.web().audit.record(
            entry
                .clone()
                .outcome(format!("refused: {}", crate::upload::refusal(&resp))),
        );
        resp
    };
    let len = match crate::upload::declared_length(&req, packs.max_upload(), "data packs") {
        Ok(len) => len,
        Err(resp) => return refuse(resp),
    };
    let name = match req.query_param("name").filter(|n| !n.is_empty()) {
        Some(name) => name,
        None => match packs.suggested_name(&shown) {
            Some(name) => name,
            None => {
                return refuse(error(
                    StatusCode::BAD_REQUEST,
                    "name the pack (?name=) or give its file name (?filename=)",
                ));
            }
        },
    };
    let _ticket = match transfers.begin(&ctx.principal.name(), false).await {
        Ok(ticket) => ticket,
        Err(Busy::Yours) => {
            return error(StatusCode::TOO_MANY_REQUESTS, "you are uploading already");
        }
        Err(_) => {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "too many uploads are running; try again later",
            );
        }
    };
    if let Some(dir) = host.world_dir()
        && let Some(Err(resp)) = blocking(move || crate::upload::check_space(&dir, len)).await
    {
        return refuse(resp);
    }
    let bytes = match crate::upload::receive(&mut req, len).await {
        Ok(bytes) => bytes,
        Err(resp) => return refuse(resp),
    };
    let replace = crate::upload::flag(&req, "replace");
    let enable_it = crate::upload::flag(&req, "enable");
    match packs.add(&name, bytes, replace, enable_it).await {
        Ok((id, body)) => {
            host.web()
                .audit
                .record(entry.target(id).detail(format!("{shown} ({len} bytes)")));
            Response::json(StatusCode::OK, &body)
        }
        Err(err) => refuse(failure(&err)),
    }
}
