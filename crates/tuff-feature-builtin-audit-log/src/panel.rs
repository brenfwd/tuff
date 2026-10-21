//! A read-only admin panel API over the audit log: `GET
//! /admin/api/v1/plugins/audit-log` lists lookups, gated by
//! [`READ_NODE`]. Query parameters are the `/tuff audit lookup` ones (`u`,
//! `t`, `a`, `i`, `e`, `r` as `#world`) plus `limit` and `offset`.

use serde_json::json;
use tuff_core::registry::Registries;
use tuff_http::Response;
use tuff_item::load::LoadError;
use tuff_web_ui::auth::Scope;
use tuff_web_ui::panel::{ApiCall, Method, PanelApi, StatusCode, api_handler};

use crate::filter::{self, Origin};
use crate::record::now_ms;
use crate::service::AuditLog;
use crate::store::{Order, Row};

pub const READ_NODE: &str = "tuff.audit.panel.read";
pub const API_ID: &str = "audit-log";
const MAX_LIMIT: usize = 200;

pub fn register(registries: &mut Registries) -> Result<(), LoadError> {
    registries.panel.register_api(
        crate::ID,
        PanelApi::new(API_ID, Scope::Plugin(READ_NODE), api_handler(handle)),
    )
}

fn fail(status: StatusCode, message: &str) -> Response {
    Response::json(status, &json!({ "error": message }))
}

/// The lookup words a request's query spells, in command syntax.
fn command_text(call: &ApiCall) -> String {
    ["u", "t", "a", "i", "e", "r"]
        .into_iter()
        .filter_map(|key| {
            let value = call.request.query_param(key)?;
            let value = value.trim();
            (!value.is_empty() && !value.contains(char::is_whitespace))
                .then(|| format!("{key}:{value}"))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn row_json(row: &Row) -> serde_json::Value {
    json!({
        "id": row.id,
        "time": row.time,
        "user": row.user,
        "action": row.kind.map(|kind| kind.name()),
        "world": row.world,
        "pos": row.pos,
        "material": row.material,
        "amount": row.amount,
        "text": row.text,
        "rolledBack": row.rolled_back,
    })
}

async fn handle(call: ApiCall) -> Response {
    if call.request.method != Method::GET || !matches!(call.request.rest.as_str(), "" | "/") {
        return fail(StatusCode::NOT_FOUND, "no such endpoint");
    }
    let shared = call.server::<tuff_core::world::Shared>();
    let Some(log) = shared.as_ref().and_then(|s| s.services.get::<AuditLog>()) else {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "the audit log is not running",
        );
    };
    let origin = Origin {
        world: "minecraft:overworld".to_owned(),
        pos: [0, 0, 0],
    };
    let params = match filter::parse(&command_text(&call), &origin, now_ms()) {
        Ok(params) => params,
        Err(err) => return fail(StatusCode::BAD_REQUEST, &err),
    };
    let number = |key: &str| {
        call.request
            .query_param(key)
            .and_then(|v| v.parse::<usize>().ok())
    };
    let limit = number("limit").unwrap_or(50).clamp(1, MAX_LIMIT);
    let offset = number("offset").unwrap_or(0);
    let total = match log.count(params.filter.clone()).await {
        Ok(total) => total,
        Err(err) => return fail(StatusCode::SERVICE_UNAVAILABLE, &err),
    };
    match log
        .lookup(params.filter, Order::NewestFirst, limit, offset)
        .await
    {
        Ok(rows) => Response::json(
            StatusCode::OK,
            &json!({ "total": total, "rows": rows.iter().map(row_json).collect::<Vec<_>>() }),
        ),
        Err(err) => fail(StatusCode::SERVICE_UNAVAILABLE, &err),
    }
}
