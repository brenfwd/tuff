//! The Permissions page's API over the permission store:
//!
//! - `GET /permissions`: registered nodes, player settings and roles.
//! - `POST /permissions/changes` with one change, e.g.
//!   `{"kind":"player","player":"<uuid or online name>","node":"a.b","held":true}`
//!   (`held` null clears), `{"kind":"role","role":"mods","node":"a.b","held":false}`,
//!   `{"kind":"create_role","role":"mods","priority":5}`,
//!   `{"kind":"delete_role","role":"mods"}`,
//!   `{"kind":"priority","role":"mods","priority":1}` or
//!   `{"kind":"member","player":"...","role":"mods","member":true}`.
//!
//! Every change is audited and refreshes the command trees of the online
//! players it affects.

use std::sync::Arc;

use hyper::{Method, StatusCode};
use serde_json::{Value, json};

use super::api::{json_body, method_not_allowed, require};
use super::audit::Entry;
use super::auth::{AuthContext, Scope};
use super::guard::{self, error};
use super::host::{Edit, HostFuture, PanelHost};
use tuff_http::{Request, Response};

/// What the Permissions page needs from the server.
pub trait PermissionHost: Send + Sync {
    /// Registered nodes, player settings and roles.
    fn state(&self) -> Value;
    /// Applies a change body, answering the audit action and the edit:
    /// `Ok(false)` when the settings already were that way.
    fn change<'a>(&'a self, body: &'a Value) -> HostFuture<'a, (&'static str, Edit<bool>)>;
}

pub async fn handle(host: Arc<dyn PanelHost>, req: Request) -> Response {
    let ctx = match require(&req, Some(Scope::Permissions)) {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    match (req.rest.trim_end_matches('/'), &req.method) {
        ("", &Method::GET) => Response::json(StatusCode::OK, &host.permission_settings().state()),
        ("/changes", &Method::POST) => change(&*host, &req, &ctx).await,
        ("" | "/changes", _) => method_not_allowed(),
        _ => error(StatusCode::NOT_FOUND, "no such endpoint"),
    }
}

async fn change(host: &dyn PanelHost, req: &Request, ctx: &AuthContext) -> Response {
    let body = match json_body(req) {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let settings = host.permission_settings();
    let (action, edit) = settings.change(&body).await;
    let entry = edit.detail.map(|detail| {
        let entry = Entry::new(ctx.principal.name(), action)
            .remote(guard::client_ip(host, req))
            .target(edit.target);
        if detail.is_empty() {
            entry
        } else {
            entry.detail(detail)
        }
    });
    match edit.result {
        Ok(changed) => {
            if changed && let Some(entry) = entry {
                host.web().audit.record(entry);
            }
            Response::json(
                StatusCode::OK,
                &json!({ "changed": changed, "state": settings.state() }),
            )
        }
        Err(err) => {
            if let Some(entry) = entry {
                host.web().audit.record(entry.outcome(err.message.clone()));
            }
            error(err.status, &err.message)
        }
    }
}
