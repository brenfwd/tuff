//! The World page's API:
//!
//! - `GET /world` (`players.read`): backend, spawn, time, weather,
//!   difficulty, dimensions with their borders, loaded totals, data pack
//!   and backup summaries; the seed only for owners.
//! - `POST /world/save` (`config.write`) runs `save-all` at the user's
//!   level.

use std::sync::Arc;

use hyper::{Method, StatusCode};
use serde_json::{Value, json};

use super::api::{method_not_allowed, require};
use super::audit::Entry;
use super::auth::{AuthContext, Scope};
use super::guard::{self, error};
use super::host::PanelHost;
use tuff_http::{Request, Response};
use tuff_storage::admin::PermissionLevel;

/// Whether the seed may be shown: only to owners.
pub fn sees_seed(ctx: &AuthContext) -> bool {
    ctx.level >= PermissionLevel::Owners
}

pub async fn overview(host: &dyn PanelHost, ctx: &AuthContext) -> Value {
    let mut body = host.world_summary().await;
    body["seed"] = json!(sees_seed(ctx).then(|| host.seed().to_string()));
    body["canSave"] = json!(ctx.allows(Scope::ConfigWrite));
    body
}

pub async fn handle(shared: Arc<dyn PanelHost>, req: Request) -> Response {
    let ctx = match require(&req, Some(Scope::PlayersRead)) {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    match (req.rest.trim_end_matches('/'), &req.method) {
        ("", &Method::GET) => Response::json(StatusCode::OK, &overview(&*shared, &ctx).await),
        ("/save", &Method::POST) => {
            if let Err(resp) = ctx.require(Scope::ConfigWrite) {
                return resp;
            }
            let (feedback, ok) =
                super::run_command(shared.clone(), ctx.principal.clone(), ctx.level, "save-all")
                    .await;
            let mut entry = Entry::new(ctx.principal.name(), "world.save")
                .remote(guard::client_ip(&*shared, &req));
            if !ok {
                entry = entry.outcome(feedback.first().cloned().unwrap_or_else(|| "failed".into()));
            }
            shared.web().audit.record(entry);
            Response::json(StatusCode::OK, &json!({ "ok": ok, "feedback": feedback }))
        }
        ("" | "/save", _) => method_not_allowed(),
        _ => error(StatusCode::NOT_FOUND, "no such endpoint"),
    }
}
