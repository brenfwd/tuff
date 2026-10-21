//! The Resource packs page's API over the pack actor, and the packs
//! operators upload, kept in the server file area as
//! `resourcepacks/<uuid>.zip` beside a `<uuid>.json` record (written last,
//! so a zip without a record is an interrupted upload and is deleted at the
//! next start) and hosted again at every start.
//!
//! - `GET /resourcepacks`: every hosted pack (plugin, custom content and
//!   uploaded; only uploaded ones are deletable or editable) and each
//!   online player's status per pack (`resourcepacks.read`).
//! - `GET /resourcepacks/<uuid>`: the pack's bytes (`resourcepacks.read`).
//! - `PUT /resourcepacks/upload?filename=<file>&name=<name>&push=1`: a zip
//!   with a `Content-Length`, checked completely with the strict zip
//!   reader (`resourcepacks.manage`).
//! - `POST /resourcepacks/<uuid>/settings` with any of `{"name",
//!   "required", "prompt", "offerOnJoin", "order"}` (`resourcepacks.manage`).
//! - `POST /resourcepacks/<uuid>/push` and `/pop` with an optional
//!   `{"player": <uuid or online name>}`, else everyone online
//!   (`resourcepacks.manage`).
//! - `DELETE /resourcepacks/<uuid>` unhosts and deletes an uploaded pack
//!   (`resourcepacks.manage`).
//!
//! Composition: join offers go out by ascending `order` (then name), and a
//! client stacks later packs above earlier ones. Uploaded packs take an
//! order in [`ORDERS`]; plugin packs default to 0; the custom content pack
//! is always last ([`CUSTOM_CONTENT_ORDER`]), so no uploaded pack can paint
//! over custom content at join. The custom content pack is essential: it
//! cannot be deleted, popped or reconfigured here. A pack pushed during play
//! lands on top of what the client holds until it rejoins.

use std::sync::Arc;

use bytes::Bytes;
use hyper::{Method, StatusCode};
use serde_json::{Value, json};

use super::api::{method_not_allowed, require};
use super::audit::Entry;
use super::auth::{AuthContext, Scope};
use super::backups::{Busy, Transfers, blocking, read_json};
use super::guard::{self, error};
use super::host::{HostError, HostFuture, PanelHost};
use tuff_http::{Request, Response};
use tuff_storage::admin::format_uuid;

/// What the Resource packs page needs from the server.
pub trait ResourcePackHost: Send + Sync {
    /// Every hosted pack and each online player's status per pack.
    fn state(&self) -> HostFuture<'_, Value>;
    /// Whether there is a file area to keep uploaded packs in.
    fn has_file_area(&self) -> bool;
    fn max_upload(&self) -> u64;
    /// A display name cleaned for a pack, `None` if nothing is left.
    fn clean_name(&self, name: &str) -> Option<String>;
    /// Checks, stores and hosts an upload: its id, name and the answer.
    fn add<'a>(
        &'a self,
        name: &'a str,
        by: &'a str,
        bytes: Vec<u8>,
    ) -> HostFuture<'a, Result<(u128, String, Value), HostError>>;
    /// Unhosts and deletes an uploaded pack, answering its name.
    fn delete(&self, id: u128) -> HostFuture<'_, Result<String, HostError>>;
    /// Applies a settings body to an uploaded pack.
    fn configure<'a>(
        &'a self,
        id: u128,
        body: &'a Value,
    ) -> HostFuture<'a, Result<Value, HostError>>;
    /// Offers a pack to one online player (uuid or name) or everyone.
    fn push<'a>(&'a self, id: u128, who: Option<&'a str>) -> HostFuture<'a, Result<(), HostError>>;
    fn pop<'a>(&'a self, id: u128, who: Option<&'a str>) -> HostFuture<'a, Result<(), HostError>>;
    /// A hosted pack's name and built bytes.
    fn fetch(&self, id: u128) -> HostFuture<'_, Result<(String, Bytes), HostError>>;
}

fn failure(err: &HostError) -> Response {
    error(err.status, &err.message)
}

fn parse_uuid(text: &str) -> Option<u128> {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u128::from_str_radix(&hex, 16).ok()
}

pub async fn handle(host: Arc<dyn PanelHost>, req: Request, transfers: Transfers) -> Response {
    let ctx = match require(&req, Some(Scope::ResourcePacksRead)) {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    let rest = req.rest.trim_end_matches('/').to_owned();
    let method = req.method.clone();
    let parts: Vec<&str> = rest.split('/').skip(1).collect();
    let scope = match (parts.as_slice(), &method) {
        ([], &Method::GET) => Scope::ResourcePacksRead,
        (["upload"], &Method::PUT | &Method::POST) => Scope::ResourcePacksManage,
        ([] | ["upload"], _) => return method_not_allowed(),
        ([_], &Method::GET | &Method::HEAD) => Scope::ResourcePacksRead,
        ([_], &Method::DELETE) => Scope::ResourcePacksManage,
        ([_, "settings" | "push" | "pop"], &Method::POST) => Scope::ResourcePacksManage,
        ([_] | [_, "settings" | "push" | "pop"], _) => return method_not_allowed(),
        _ => return error(StatusCode::NOT_FOUND, "no such endpoint"),
    };
    if let Err(resp) = ctx.require(scope) {
        return resp;
    }
    let host = &*host;
    if parts.is_empty() {
        return Response::json(StatusCode::OK, &host.resource_packs().state().await);
    }
    if parts == ["upload"] {
        return upload_pack(host, req, &ctx, &transfers).await;
    }
    let Some(id) = parse_uuid(parts[0]) else {
        return error(StatusCode::NOT_FOUND, "no such resource pack");
    };
    match (parts.get(1).copied(), &method) {
        (None, &Method::DELETE) => remove(host, &req, &ctx, id).await,
        (None, _) => download(host, &req, &ctx, id).await,
        (Some("settings"), _) => settings(host, req, &ctx, id).await,
        (Some(action), _) => push_or_pop(host, req, &ctx, id, action == "push").await,
    }
}

fn audit(host: &dyn PanelHost, req: &Request, ctx: &AuthContext, action: &str) -> Entry {
    Entry::new(ctx.principal.name(), action).remote(guard::client_ip(host, req))
}

fn refused(host: &dyn PanelHost, entry: Entry, err: &HostError) -> Response {
    if !err.quiet {
        host.web().audit.record(entry.outcome(err.message.clone()));
    }
    failure(err)
}

async fn upload_pack(
    host: &dyn PanelHost,
    mut req: Request,
    ctx: &AuthContext,
    transfers: &Transfers,
) -> Response {
    let packs = host.resource_packs();
    let shown = crate::upload::shown_name(&req);
    let entry = audit(host, &req, ctx, "resourcepack.upload").detail(shown.clone());
    let refuse = |resp: Response| {
        host.web().audit.record(
            entry
                .clone()
                .outcome(format!("refused: {}", crate::upload::refusal(&resp))),
        );
        resp
    };
    if !packs.has_file_area() {
        return refuse(error(
            StatusCode::CONFLICT,
            "this server has no file area to keep uploaded packs in",
        ));
    }
    let len = match crate::upload::declared_length(&req, packs.max_upload(), "resource packs") {
        Ok(len) => len,
        Err(resp) => return refuse(resp),
    };
    let name = req
        .query_param("name")
        .and_then(|n| packs.clean_name(&n))
        .or_else(|| {
            host.datapacks()
                .suggested_name(&shown)
                .map(|n| n.strip_suffix(".zip").unwrap_or(&n).to_owned())
        })
        .unwrap_or_else(|| "Uploaded pack".to_owned());
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
    let (id, name, mut body) = match packs.add(&name, &ctx.principal.name(), bytes).await {
        Ok(added) => added,
        Err(err) => return refuse(failure(&err)),
    };
    let pushed = crate::upload::flag(&req, "push") && packs.push(id, None).await.is_ok();
    host.web().audit.record(
        entry
            .target(format_uuid(id))
            .detail(format!("{shown} as {name} ({len} bytes)")),
    );
    body["pushed"] = json!(pushed);
    Response::json(StatusCode::OK, &body)
}

async fn remove(host: &dyn PanelHost, req: &Request, ctx: &AuthContext, id: u128) -> Response {
    let entry = audit(host, req, ctx, "resourcepack.delete").target(format_uuid(id));
    match host.resource_packs().delete(id).await {
        Ok(name) => {
            host.web().audit.record(entry.detail(name));
            Response::json(StatusCode::OK, &json!({ "deleted": format_uuid(id) }))
        }
        Err(err) => refused(host, entry, &err),
    }
}

async fn settings(host: &dyn PanelHost, mut req: Request, ctx: &AuthContext, id: u128) -> Response {
    let body = match read_json(&mut req).await {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let entry = audit(host, &req, ctx, "resourcepack.settings")
        .target(format_uuid(id))
        .detail(body.to_string());
    match host.resource_packs().configure(id, &body).await {
        Ok(answer) => {
            host.web().audit.record(entry);
            Response::json(StatusCode::OK, &answer)
        }
        Err(err) => refused(host, entry, &err),
    }
}

async fn push_or_pop(
    host: &dyn PanelHost,
    mut req: Request,
    ctx: &AuthContext,
    id: u128,
    pushing: bool,
) -> Response {
    let body = match read_json(&mut req).await {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let who = body["player"].as_str().map(str::to_owned);
    let action = if pushing {
        "resourcepack.push"
    } else {
        "resourcepack.pop"
    };
    let entry = audit(host, &req, ctx, action)
        .target(format_uuid(id))
        .detail(who.clone().unwrap_or_else(|| "everyone".to_owned()));
    let packs = host.resource_packs();
    let result = if pushing {
        packs.push(id, who.as_deref()).await
    } else {
        packs.pop(id, who.as_deref()).await
    };
    match result {
        Ok(()) => {
            host.web().audit.record(entry);
            Response::json(StatusCode::OK, &json!({ "ok": true }))
        }
        Err(err) => refused(host, entry, &err),
    }
}

async fn download(host: &dyn PanelHost, req: &Request, ctx: &AuthContext, id: u128) -> Response {
    let (name, bytes) = match host.resource_packs().fetch(id).await {
        Ok(found) => found,
        Err(err) => return failure(&err),
    };
    let mut resp = Response::conditional(req, "application/zip", bytes);
    crate::upload::attachment(&mut resp, &format!("{name}.zip"));
    if req.method == Method::GET && req.header("range").is_none() && resp.status == StatusCode::OK {
        host.web().audit.record(
            audit(host, req, ctx, "resourcepack.download")
                .target(format_uuid(id))
                .detail(name),
        );
    }
    resp
}
