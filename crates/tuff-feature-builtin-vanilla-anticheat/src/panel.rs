//! The web panel's view of the violation board: `GET
//! /admin/api/v1/plugins/anticheat[?player=<uuid>]` lists each player's
//! per-check history, gated by the `/anticheat violations` node. The
//! Players page reads it when the API exists.

use std::time::Instant;

use serde_json::{Value, json};
use tuff_core::registry::Registries;
use tuff_http::Response;
use tuff_item::load::LoadError;
use tuff_storage::admin::{format_uuid, parse_uuid_text};
use tuff_web_ui::Scope;
use tuff_web_ui::panel::{ApiCall, Method, PanelApi, StatusCode, api_handler};

use crate::permissions::VIOLATIONS;
use crate::service::{Board, DAY, HOUR, PlayerStats, totals};

/// The API's id, so its route is `/admin/api/v1/plugins/anticheat`.
pub const API: &str = "anticheat";

pub fn register(registries: &mut Registries, feature: &'static str) -> Result<(), LoadError> {
    registries.panel.register_api(
        feature,
        PanelApi::new(API, Scope::Plugin(VIOLATIONS), api_handler(violations)),
    )
}

/// One player's history as JSON, ages measured from `now`.
pub fn stats_json(stats: &PlayerStats, now: Instant) -> Value {
    let checks: Vec<Value> = stats
        .checks
        .iter()
        .map(|(check, c)| {
            json!({
                "check": check,
                "flags": c.flags,
                "kicks": c.kicks,
                "weight": c.weight,
                "level": c.level,
                "lastHour": c.recent.within(now, HOUR),
                "lastDay": c.recent.within(now, DAY),
                "agoMs": now.saturating_duration_since(c.last).as_millis() as u64,
            })
        })
        .collect();
    let (hour, day) = stats.checks.values().fold((0, 0), |(h, d), c| {
        (
            h + c.recent.within(now, HOUR),
            d + c.recent.within(now, DAY),
        )
    });
    json!({
        "lastHour": hour,
        "lastDay": day,
        "uuid": format_uuid(stats.player),
        "name": &*stats.name,
        "flags": stats.flags(),
        "kicks": stats.kicks(),
        "checks": checks,
    })
}

fn empty(summary: bool) -> Value {
    if summary {
        json!({ "checks": [] })
    } else {
        json!({ "players": [] })
    }
}

async fn violations(call: ApiCall) -> Response {
    let Some(shared) = call.server::<tuff_core::world::Shared>() else {
        return Response::status(StatusCode::SERVICE_UNAVAILABLE);
    };
    let request = call.request;
    let summary = request.rest == "/summary";
    if request.method != Method::GET || !(summary || matches!(request.rest.as_str(), "" | "/")) {
        return Response::json(
            StatusCode::NOT_FOUND,
            &json!({ "error": "no such endpoint" }),
        );
    }
    let Some(board) = shared.services.get::<Board>() else {
        return Response::json(StatusCode::OK, &empty(summary));
    };
    if summary {
        let now = Instant::now();
        let all = board.list().await;
        let checks: Vec<Value> = totals(&all, now)
            .iter()
            .map(|(check, t)| {
                json!({
                    "check": check,
                    "flags": t.flags,
                    "kicks": t.kicks,
                    "lastHour": t.hour,
                    "lastDay": t.day,
                })
            })
            .collect();
        return Response::json(StatusCode::OK, &json!({ "checks": checks }));
    }
    let only = request
        .query_param("player")
        .and_then(|p| parse_uuid_text(&p));
    let now = Instant::now();
    let players: Vec<Value> = board
        .list()
        .await
        .iter()
        .filter(|stats| only.is_none_or(|uuid| stats.player == uuid))
        .map(|stats| stats_json(stats, now))
        .collect();
    Response::json(StatusCode::OK, &json!({ "players": players }))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use super::*;
    use crate::service::CheckStats;

    #[tuff_core::watchdog]
    #[test]
    fn without_a_board_each_endpoint_answers_its_own_empty_shape() {
        assert_eq!(empty(true), json!({ "checks": [] }));
        assert_eq!(empty(false), json!({ "players": [] }));
    }

    #[tuff_core::watchdog]
    #[test]
    fn stats_list_every_check() {
        let now = Instant::now();
        let stats = PlayerStats {
            player: 0x12,
            name: Arc::from("Alex"),
            checks: BTreeMap::from([(
                "fly",
                CheckStats {
                    flags: 3,
                    weight: 2.5,
                    kicks: 1,
                    level: 4.0,
                    last: now,
                    recent: Default::default(),
                },
            )]),
        };
        let value = stats_json(&stats, now);
        assert_eq!(value["name"], "Alex");
        assert_eq!(value["flags"], 3);
        assert_eq!(value["kicks"], 1);
        assert_eq!(value["checks"][0]["check"], "fly");
        assert_eq!(value["checks"][0]["agoMs"], 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_api_is_registered_behind_the_violations_node() {
        let mut registries = Registries::new();
        register(&mut registries, "vanilla-anticheat").unwrap();
        let installed = registries.panel.installed();
        let api = installed.apis.get(API).unwrap();
        assert_eq!(api.scope, Scope::Plugin(VIOLATIONS));
        assert_eq!(api.route(), "/admin/api/v1/plugins/anticheat");
    }
}
