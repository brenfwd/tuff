//! The Config page's API:
//!
//! - `GET /config` (`config.read`): every runtime setting with its kind,
//!   default, description, hints and owning plugin, the server.properties
//!   entries and the game rules. Secret settings and properties only say
//!   whether they are set.
//! - `PUT /config/<key>`, `/config/properties/<key>` and
//!   `/config/gamerules/<rule>` (`config.write`) with `{"value": "..."}`
//!   or `{"reset": true}`. Settings go through the config store, like
//!   `/tuff config`, so validation and live reload apply; game rules run
//!   `/gamerule` at the user's level. A change that could lock the editor
//!   out of the panel answers 409 unless the body has `"confirm": true`.

use std::sync::Arc;

use hyper::{Method, StatusCode};
use serde_json::{Value as Json, json};

use super::api::{json_body, method_not_allowed, require};
use super::audit::Entry;
use super::auth::{AuthContext, Scope};
use super::guard::{self, error};
use super::host::{Edit, HostError, HostFuture, PanelHost};
use tuff_http::{Request, Response};

/// What the audit log and the API show instead of a secret value.
pub const REDACTED: &str = "<redacted>";

/// A checked game rule change: the command that applies it.
pub struct RulePlan {
    pub line: String,
    /// Whether the rule holds another value now.
    pub changed: bool,
}

/// What the Config page needs from the server.
pub trait ConfigHost: Send + Sync {
    /// Every runtime setting; secret ones only say whether they are set.
    fn settings(&self) -> Vec<Json>;
    /// Sets (or with `None` resets) a setting through the config store.
    fn set_setting(&self, name: &str, value: Option<String>, confirm: bool) -> Edit<Json>;
    /// The server.properties entries, `None` while the world is down.
    fn properties(&self) -> HostFuture<'_, Option<Vec<Json>>>;
    fn set_property<'a>(
        &'a self,
        name: &'a str,
        value: Option<String>,
    ) -> HostFuture<'a, Edit<Json>>;
    /// The enabled game rules with their values.
    fn game_rules(&self) -> Vec<Json>;
    /// Checks a game rule change against the rule's type and bounds.
    fn plan_rule(&self, name: &str, value: Option<String>) -> Edit<RulePlan>;
}

pub async fn handle(host: Arc<dyn PanelHost>, req: Request) -> Response {
    let ctx = match require(&req, Some(Scope::ConfigRead)) {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    let rest = req.rest.trim_end_matches('/').to_owned();
    if rest.is_empty() {
        if req.method != Method::GET {
            return method_not_allowed();
        }
        return state(&*host, &ctx).await;
    }
    if req.method != Method::PUT {
        return method_not_allowed();
    }
    if let Err(resp) = ctx.require(Scope::ConfigWrite) {
        return resp;
    }
    let body = match json_body(&req) {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let change = match Change::parse(&body) {
        Ok(change) => change,
        Err(text) => return error(StatusCode::BAD_REQUEST, text),
    };
    let rest = rest.trim_start_matches('/');
    let config = host.config();
    match rest.split_once('/') {
        Some(("properties", name)) => {
            let edit = config.set_property(name, change.value).await;
            finish(&*host, &req, &ctx, "properties.set", edit)
        }
        Some(("gamerules", name)) => set_rule(&host, &req, &ctx, name, change).await,
        Some(_) => error(StatusCode::NOT_FOUND, "no such endpoint"),
        None => {
            let edit = config.set_setting(rest, change.value.clone(), change.confirm);
            let action = if change.value.is_some() {
                "config.set"
            } else {
                "config.reset"
            };
            finish(&*host, &req, &ctx, action, edit)
        }
    }
}

async fn state(host: &dyn PanelHost, ctx: &AuthContext) -> Response {
    let config = host.config();
    let Some(properties) = config.properties().await else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "the world is not running");
    };
    Response::json(
        StatusCode::OK,
        &json!({
            "settings": config.settings(),
            "properties": properties,
            "gameRules": config.game_rules(),
            "writable": ctx.allows(Scope::ConfigWrite),
        }),
    )
}

/// What a PUT asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The new value's text; `None` resets to the default.
    pub value: Option<String>,
    pub confirm: bool,
}

impl Change {
    pub fn parse(body: &Json) -> Result<Change, &'static str> {
        let confirm = body.get("confirm").and_then(Json::as_bool).unwrap_or(false);
        if body.get("reset").and_then(Json::as_bool) == Some(true) {
            return Ok(Change {
                value: None,
                confirm,
            });
        }
        let value = match body.get("value") {
            Some(Json::String(s)) => s.clone(),
            Some(Json::Bool(b)) => b.to_string(),
            Some(Json::Number(n)) => n.to_string(),
            _ => return Err("value (a string) or reset is required"),
        };
        Ok(Change {
            value: Some(value),
            confirm,
        })
    }
}

fn entry(
    host: &dyn PanelHost,
    req: &Request,
    ctx: &AuthContext,
    action: &str,
    target: &str,
) -> Entry {
    Entry::new(ctx.principal.name(), action)
        .remote(guard::client_ip(host, req))
        .target(target)
}

/// Audits an edit and answers it.
fn finish(
    host: &dyn PanelHost,
    req: &Request,
    ctx: &AuthContext,
    action: &str,
    edit: Edit<Json>,
) -> Response {
    let record = edit
        .detail
        .map(|detail| entry(host, req, ctx, action, &edit.target).detail(detail));
    match edit.result {
        Ok(body) => {
            if let Some(record) = record {
                host.web().audit.record(record);
            }
            Response::json(StatusCode::OK, &body)
        }
        Err(err) => {
            if let Some(record) = record {
                host.web().audit.record(record.outcome(err.message.clone()));
            }
            refusal(&err)
        }
    }
}

fn refusal(err: &HostError) -> Response {
    if err.confirm {
        return Response::json(
            StatusCode::CONFLICT,
            &json!({ "error": err.message, "confirm": true }),
        );
    }
    error(err.status, &err.message)
}

async fn set_rule(
    host: &Arc<dyn PanelHost>,
    req: &Request,
    ctx: &AuthContext,
    name: &str,
    change: Change,
) -> Response {
    let action = "gamerules.set";
    let Edit {
        target,
        detail,
        result,
    } = host.config().plan_rule(name, change.value);
    let plan = match result {
        Ok(plan) => plan,
        Err(err) => {
            if let Some(detail) = detail {
                host.web().audit.record(
                    entry(&**host, req, ctx, action, &target)
                        .detail(detail)
                        .outcome(err.message.clone()),
                );
            }
            return refusal(&err);
        }
    };
    if plan.changed {
        let (feedback, ok) =
            super::run_command(host.clone(), ctx.principal.clone(), ctx.level, &plan.line).await;
        let mut record =
            entry(&**host, req, ctx, action, &target).detail(detail.unwrap_or_default());
        if !ok {
            let first = feedback
                .first()
                .cloned()
                .unwrap_or_else(|| "failed".to_owned());
            record = record.outcome(first.clone());
            host.web().audit.record(record);
            return error(StatusCode::FORBIDDEN, &first);
        }
        host.web().audit.record(record);
    }
    let row = host
        .config()
        .game_rules()
        .into_iter()
        .find(|r| r["name"] == target.as_str())
        .unwrap_or(Json::Null);
    Response::json(StatusCode::OK, &row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn changes_parse_values_resets_and_confirmation() {
        assert_eq!(
            Change::parse(&json!({"value": 5, "confirm": true})),
            Ok(Change {
                value: Some("5".into()),
                confirm: true
            })
        );
        assert_eq!(
            Change::parse(&json!({"reset": true})),
            Ok(Change {
                value: None,
                confirm: false
            })
        );
        assert!(Change::parse(&json!({})).is_err());
    }
}
