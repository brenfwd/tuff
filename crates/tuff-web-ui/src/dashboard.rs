//! The dashboard's API: the latest metrics snapshot (`GET /metrics`),
//! series history (`GET /metrics/history?series=a,b&range=1h`), and the
//! bridge pushing every snapshot to the `metrics` event topic.

use std::sync::Arc;
use std::time::Duration;

use hyper::{Method, StatusCode};
use serde_json::{Map, Value, json};
use tokio::sync::watch;

use super::api::{method_not_allowed, require};
use super::auth::Scope;
use super::events::{self, PanelEvents};
use super::guard::error;
use super::host::PanelHost;
use super::panel::Installed;
use tuff_http::{Request, Response};

/// Series one history request may ask for.
pub const MAX_SERIES: usize = 8;
/// Alerts the snapshot answer carries for the feed.
pub const RECENT_ALERTS: usize = 50;
const MAX_SERIES_NAME: usize = 128;

/// One sample, or one minute's rollup: the mean and the largest value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub unix_ms: u64,
    pub value: f64,
    pub max: f64,
}

/// Dotted lowercase names like `map.tiles_rendered`.
pub fn valid_series_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_SERIES_NAME
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// `90`, `90s`, `10m`, `1h` or `1d`, capped at `max`.
pub fn parse_range(text: &str, max: Duration) -> Option<Duration> {
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (digits, unit) = text.split_at(split);
    let n: u64 = digits.parse().ok()?;
    let scale = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return None,
    };
    let secs = n.checked_mul(scale)?;
    (secs > 0).then(|| Duration::from_secs(secs).min(max))
}

/// One snapshot as the dashboard receives it, over the API and the
/// `metrics` topic alike.
pub fn live_json(snapshot: Value, audit_failures: u64) -> Value {
    json!({
        "snapshot": snapshot,
        "panel": { "auditFailures": audit_failures },
    })
}

/// Points as `[unix_ms, value, max]` triples.
pub fn points_json(points: &[Point]) -> Value {
    Value::Array(
        points
            .iter()
            .map(|p| json!([p.unix_ms, p.value, p.max]))
            .collect(),
    )
}

pub async fn metrics(shared: Arc<dyn PanelHost>, req: Request, panel: Arc<Installed>) -> Response {
    let ctx = match require(&req, Some(Scope::MetricsRead)) {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    if req.method != Method::GET {
        return method_not_allowed();
    }
    match req.rest.as_str() {
        "" | "/" => {
            let snapshot = shared.metrics_snapshot();
            let mut body = live_json(snapshot, shared.web().audit.failures());
            let cards: Vec<Value> = panel
                .cards_for(&ctx)
                .into_iter()
                .map(|c| json!({ "series": c.series, "title": c.title, "unit": c.unit }))
                .collect();
            body["cards"] = Value::Array(cards);
            let alerts_allowed =
                events::topic_scope(events::ALERTS).is_some_and(|scope| ctx.allows(scope));
            let alerts = if alerts_allowed {
                shared
                    .web()
                    .events
                    .recent(events::ALERTS, RECENT_ALERTS)
                    .await
                    .iter()
                    .filter_map(|p| serde_json::from_str::<Value>(&p.event.data).ok())
                    .collect()
            } else {
                Vec::new()
            };
            body["alerts"] = Value::Array(alerts);
            let (seconds, minutes) = shared.history_span();
            body["history"] = json!({
                "seconds": seconds.as_secs(),
                "minutes": minutes.as_secs() / 60,
            });
            Response::json(StatusCode::OK, &body)
        }
        "/history" => history(&*shared, &req).await,
        _ => error(StatusCode::NOT_FOUND, "no such endpoint"),
    }
}

async fn history(shared: &dyn PanelHost, req: &Request) -> Response {
    let wanted = req.query_param("series").unwrap_or_default();
    let names: Vec<&str> = wanted
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if names.is_empty() || names.len() > MAX_SERIES {
        return error(
            StatusCode::BAD_REQUEST,
            "name 1 to 8 series, comma separated",
        );
    }
    if !names.iter().all(|n| valid_series_name(n)) {
        return error(StatusCode::BAD_REQUEST, "bad series name");
    }
    let (default_range, max_range) = shared.history_span();
    let range = match req.query_param("range") {
        None => default_range,
        Some(text) => match parse_range(&text, max_range) {
            Some(range) => range,
            None => return error(StatusCode::BAD_REQUEST, "bad range"),
        },
    };
    let mut series = Map::new();
    for name in names {
        let points = shared.metrics_history(name, range).await;
        series.insert(
            name.to_owned(),
            points.map_or(Value::Null, |p| points_json(&p)),
        );
    }
    let step = if range <= default_range { 1 } else { 60 };
    Response::json(
        StatusCode::OK,
        &json!({ "range": range.as_secs(), "step": step, "series": series }),
    )
}

/// What the snapshot bridge does with the next snapshot.
pub enum Gate {
    Publish {
        audit_failures: u64,
    },
    /// The panel is off: nobody can be listening.
    Skip,
    /// The server is gone.
    Stop,
}

/// Publishes every snapshot `latest` sees on the `metrics` topic, as
/// [`live_json`] of `to_json`, while `gate` allows it.
pub async fn forward_snapshots<T: Clone>(
    mut latest: watch::Receiver<T>,
    to_json: impl Fn(&T) -> Value,
    events: PanelEvents,
    gate: impl Fn() -> Gate,
) {
    while latest.changed().await.is_ok() {
        let snapshot = latest.borrow_and_update().clone();
        match gate() {
            Gate::Stop => return,
            Gate::Skip => {}
            Gate::Publish { audit_failures } => {
                let data = live_json(to_json(&snapshot), audit_failures).to_string();
                events.publish(events::METRICS, "snapshot", data);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX_RANGE: Duration = Duration::from_secs(1440 * 60);

    #[tuff_core::watchdog]
    #[test]
    fn ranges_and_series_names_parse() {
        assert_eq!(parse_range("90", MAX_RANGE), Some(Duration::from_secs(90)));
        assert_eq!(
            parse_range("10m", MAX_RANGE),
            Some(Duration::from_secs(600))
        );
        assert_eq!(
            parse_range(" 1h ", MAX_RANGE),
            Some(Duration::from_secs(3600))
        );
        assert_eq!(parse_range("1d", MAX_RANGE), Some(MAX_RANGE));
        assert_eq!(parse_range("30d", MAX_RANGE), Some(MAX_RANGE));
        for bad in [
            "",
            "0",
            "0m",
            "h",
            "1y",
            "-5",
            "1.5h",
            "99999999999999999999h",
        ] {
            assert_eq!(parse_range(bad, MAX_RANGE), None, "{bad:?}");
        }
        assert!(valid_series_name("tick.mspt.p95"));
        assert!(valid_series_name("net.connections.play"));
        assert!(!valid_series_name(""));
        assert!(!valid_series_name("Tick"));
        assert!(!valid_series_name("a/b"));
        assert!(!valid_series_name(&"a".repeat(MAX_SERIES_NAME + 1)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn live_payloads_wrap_the_snapshot_and_points_are_triples() {
        let live = live_json(json!({ "unix_ms": 5, "dropped_reports": 3 }), 2);
        assert_eq!(live["snapshot"]["unix_ms"], 5);
        assert_eq!(live["snapshot"]["dropped_reports"], 3);
        assert_eq!(live["panel"]["auditFailures"], 2);
        let points = points_json(&[Point {
            unix_ms: 7,
            value: 1.5,
            max: 2.0,
        }]);
        assert_eq!(points, json!([[7, 1.5, 2.0]]));
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn the_bridge_publishes_snapshots_while_the_gate_allows() {
        use crate::auth::{AuthContext, Principal};
        use tuff_storage::admin::PermissionLevel;
        let bus = PanelEvents::spawn();
        let ctx = AuthContext {
            principal: Principal::Console,
            level: PermissionLevel::Owners,
            session: "s".into(),
            csrf: String::new(),
            permissions: Arc::new(crate::auth::LevelDefaults),
        };
        let mut stream = bus.stream(&ctx, events::METRICS, None).await.unwrap();
        let (publish, latest) = watch::channel(json!({ "unix_ms": 0 }));
        let open = Arc::new(std::sync::atomic::AtomicU8::new(0));
        let gate = {
            let open = open.clone();
            move || match open.load(std::sync::atomic::Ordering::SeqCst) {
                0 => Gate::Skip,
                1 => Gate::Publish { audit_failures: 4 },
                _ => Gate::Stop,
            }
        };
        let bridge = tokio::spawn(forward_snapshots(latest, Value::clone, bus.clone(), gate));
        publish.send_replace(json!({ "unix_ms": 1 }));
        tokio::task::yield_now().await;
        open.store(1, std::sync::atomic::Ordering::SeqCst);
        publish.send_replace(json!({ "unix_ms": 2 }));
        loop {
            let event = stream.recv().await.unwrap();
            assert_eq!(event.event.as_deref(), Some("snapshot"));
            let data: Value = serde_json::from_str(&event.data).unwrap();
            assert_eq!(data["panel"]["auditFailures"], 4);
            if data["snapshot"]["unix_ms"] == 2 {
                break;
            }
        }
        open.store(2, std::sync::atomic::Ordering::SeqCst);
        publish.send_replace(json!({ "unix_ms": 3 }));
        bridge.await.unwrap();
    }
}
