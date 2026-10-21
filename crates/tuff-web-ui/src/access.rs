//! API tokens and sessions (`panel.admin`):
//!
//! - `GET /tokens` lists tokens (never their secrets); `POST /tokens`
//!   `{"name": "ci", "level": 2, "expiresIn": 86400}` makes one at most at
//!   the caller's level (expiring after `expiresIn` seconds, if given) and
//!   returns its secret once; `DELETE /tokens/<id>` revokes it and
//!   the sessions it opened at once.
//! - `GET /sessions` lists live sessions; `DELETE /sessions/<hash>` ends
//!   one and `DELETE /sessions` ends every one.

use std::sync::Arc;

use hyper::{Method, StatusCode};
use serde_json::{Value, json};

use super::api::{json_body, method_not_allowed, require};
use super::audit::Entry;
use super::auth::{AuthContext, Scope, SessionInfo, TokenInfo, valid_token_name};
use super::guard::{self, error};
use super::host::PanelHost;
use tuff_http::{Request, Response};
use tuff_storage::admin::PermissionLevel;

/// The longest a token may be made to last: ten years.
pub const MAX_EXPIRY_SECS: u64 = 10 * 365 * 24 * 3600;

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

pub fn token_json(token: &TokenInfo) -> Value {
    json!({
        "id": token.id,
        "name": token.name,
        "level": token.level.id(),
        "created": token.created,
        "lastUsed": token.last_used,
        "expires": token.expires,
        "expired": token.expired(),
        "by": token.by,
    })
}

pub fn session_json(session: &SessionInfo, current: &str) -> Value {
    json!({
        "session": session.session,
        "principal": session.principal.to_json(),
        "created": session.created,
        "lastSeen": session.last_seen,
        "remote": session.remote,
        "current": session.session == current,
    })
}

fn record(
    shared: &dyn PanelHost,
    req: &Request,
    ctx: &AuthContext,
    action: &str,
    target: &str,
) -> Entry {
    Entry::new(ctx.principal.name(), action)
        .remote(guard::client_ip(shared, req))
        .target(target)
}

pub async fn tokens(shared: Arc<dyn PanelHost>, req: Request) -> Response {
    let ctx = match require(&req, Some(Scope::PanelAdmin)) {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    let auth = &shared.web().auth;
    match (req.rest.trim_end_matches('/'), &req.method) {
        ("", &Method::GET) => {
            let list: Vec<Value> = auth.tokens().await.iter().map(token_json).collect();
            Response::json(StatusCode::OK, &json!({ "tokens": list }))
        }
        ("", &Method::POST) => {
            let body = match json_body(&req) {
                Ok(body) => body,
                Err(resp) => return resp,
            };
            let name = body.get("name").and_then(Value::as_str).unwrap_or_default();
            if !valid_token_name(name) {
                return error(
                    StatusCode::BAD_REQUEST,
                    "name must be 1 to 64 letters, digits, _, - or .",
                );
            }
            let Some(level) = body
                .get("level")
                .and_then(Value::as_i64)
                .and_then(PermissionLevel::from_id)
            else {
                return error(StatusCode::BAD_REQUEST, "level must be 0 to 4");
            };
            if level > ctx.level {
                return error(StatusCode::FORBIDDEN, "a token cannot outrank you");
            }
            let expires = match body.get("expiresIn") {
                None | Some(Value::Null) => None,
                Some(value) => match value.as_u64().filter(|s| (1..=MAX_EXPIRY_SECS).contains(s)) {
                    Some(secs) => Some(unix_millis() + secs * 1000),
                    None => {
                        return error(
                            StatusCode::BAD_REQUEST,
                            "expiresIn must be 1 to 315360000 seconds",
                        );
                    }
                },
            };
            let Some(made) = auth
                .create_token(name.to_owned(), level, expires, ctx.principal.name())
                .await
            else {
                return error(StatusCode::CONFLICT, "too many tokens");
            };
            shared.web().audit.record(
                record(&*shared, &req, &ctx, "token.create", name)
                    .detail(format!("level {}", level.id())),
            );
            Response::json(
                StatusCode::OK,
                &json!({
                    "token": made.secret,
                    "principal": made.principal.to_json(),
                    "expires": expires,
                }),
            )
        }
        ("", _) => method_not_allowed(),
        (rest, &Method::DELETE) => {
            let id = rest.trim_start_matches('/');
            if !auth.revoke_token(id).await {
                return error(StatusCode::NOT_FOUND, "no such token");
            }
            shared
                .web()
                .audit
                .record(record(&*shared, &req, &ctx, "token.revoke", id));
            Response::json(StatusCode::OK, &json!({}))
        }
        _ => method_not_allowed(),
    }
}

pub async fn sessions(shared: Arc<dyn PanelHost>, req: Request) -> Response {
    let ctx = match require(&req, Some(Scope::PanelAdmin)) {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    let auth = &shared.web().auth;
    match (req.rest.trim_end_matches('/'), &req.method) {
        ("", &Method::GET) => {
            let list: Vec<Value> = auth
                .sessions()
                .await
                .iter()
                .map(|s| session_json(s, &ctx.session))
                .collect();
            Response::json(StatusCode::OK, &json!({ "sessions": list }))
        }
        ("", &Method::DELETE) => {
            let ended = auth.logout_all().await;
            shared.web().audit.record(
                record(&*shared, &req, &ctx, "session.revoke-all", "").detail(ended.to_string()),
            );
            Response::json(StatusCode::OK, &json!({ "ended": ended }))
        }
        ("", _) => method_not_allowed(),
        (rest, &Method::DELETE) => {
            let session = rest.trim_start_matches('/');
            if !auth.revoke_session(session).await {
                return error(StatusCode::NOT_FOUND, "no such session");
            }
            shared
                .web()
                .audit
                .record(record(&*shared, &req, &ctx, "session.revoke", session));
            Response::json(StatusCode::OK, &json!({ "ended": 1 }))
        }
        _ => method_not_allowed(),
    }
}
