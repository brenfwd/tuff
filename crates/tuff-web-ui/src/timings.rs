//! The Timings page's API over the server's timings controller:
//!
//! - `GET /timings`: the status (running recording, last summary).
//! - `POST /timings/start` with optional `{"seconds": n}`; `POST
//!   /timings/stop` answers the summary.
//! - `GET /timings/reports`: saved and remembered reports, newest first.
//! - `GET /timings/reports/<id>?format=json|folded|speedscope&download=1`:
//!   one report, converted, as an attachment when `download` is set.

use std::sync::Arc;
use std::time::Duration;

use hyper::{Method, StatusCode};
use serde_json::{Value, json};

use super::api::{json_body, method_not_allowed, require};
use super::audit::Entry;
use super::auth::{AuthContext, Scope};
use super::guard::{self, error};
use super::host::PanelHost;
use tuff_http::{Request, Response};
use tuff_timings::{
    Export, Info, RecorderError, Recording, StartOptions, Status, Summary, TimingsError,
};

/// Longest recording the panel may ask for, as the command allows.
pub const MAX_SECONDS: u64 = 86_400;

pub fn recording_json(recording: &Recording) -> Value {
    json!({
        "started_unix_ms": recording.started_unix_ms,
        "elapsed_ms": recording.elapsed.as_millis() as u64,
        "threads": recording.threads,
        "stops_after_s": recording.stops_after.map(|d| d.as_secs()),
        "by": recording.by,
    })
}

pub fn summary_json(summary: &Summary) -> Value {
    json!({
        "id": summary.id,
        "file": summary.file.as_ref().ok(),
        "file_error": summary.file.as_ref().err(),
        "bytes": summary.bytes,
        "duration_ms": summary.duration.as_millis() as u64,
        "ticks": summary.ticks,
        "tps": summary.tps,
        "mspt_mean": summary.mspt_mean,
        "mspt_p99": summary.mspt_p99,
        "mspt_max": summary.mspt_max,
        "top": summary
            .top
            .iter()
            .map(|(label, ns)| json!({ "label": label, "self_ns": ns }))
            .collect::<Vec<_>>(),
        "features": summary
            .features
            .iter()
            .map(|(id, ns)| json!({ "id": id, "total_ns": ns }))
            .collect::<Vec<_>>(),
        "threads": summary.threads,
        "dropped": summary.dropped,
    })
}

pub fn status_json(status: &Status) -> Value {
    json!({
        "recording": status.recording.as_ref().map(recording_json),
        "busy_elsewhere": status.busy_elsewhere,
        "last": status.last.as_ref().map(summary_json),
    })
}

pub fn info_json(info: &Info) -> Value {
    json!({
        "id": info.id,
        "file": info.file,
        "started_unix_ms": info.started_unix_ms,
    })
}

fn failure(err: &TimingsError) -> Response {
    let status = match err {
        TimingsError::Recorder(_) | TimingsError::ElsewhereRecording => StatusCode::CONFLICT,
        TimingsError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        TimingsError::InvalidId(_) => StatusCode::BAD_REQUEST,
        TimingsError::NotFound(_) => StatusCode::NOT_FOUND,
        TimingsError::Io(_) | TimingsError::Corrupt(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    error(status, &err.to_string())
}

/// The recording length a start request asks for: `None` runs until
/// stopped.
pub fn start_duration(body: &Value) -> Result<Option<Duration>, &'static str> {
    match body.get("seconds") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => match value.as_u64() {
            Some(secs) if (1..=MAX_SECONDS).contains(&secs) => Ok(Some(Duration::from_secs(secs))),
            _ => Err("seconds must be a whole number from 1 to 86400"),
        },
    }
}

pub async fn handle(shared: Arc<dyn PanelHost>, req: Request) -> Response {
    let ctx = match require(&req, Some(Scope::Timings)) {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    let rest = req.rest.trim_end_matches('/').to_owned();
    match (rest.as_str(), &req.method) {
        ("", &Method::GET) => match shared.timings().status().await {
            Ok(status) => Response::json(StatusCode::OK, &status_json(&status)),
            Err(err) => failure(&err),
        },
        ("/start", &Method::POST) => start(&*shared, &req, &ctx).await,
        ("/stop", &Method::POST) => stop(&*shared, &req, &ctx).await,
        ("/reports", &Method::GET) => match shared.timings().list().await {
            Ok(list) => Response::json(
                StatusCode::OK,
                &json!({ "reports": list.iter().map(info_json).collect::<Vec<_>>() }),
            ),
            Err(err) => failure(&err),
        },
        ("" | "/start" | "/stop" | "/reports", _) => method_not_allowed(),
        (path, method) => match path.strip_prefix("/reports/") {
            Some(id) if !id.contains('/') => {
                if method != Method::GET && method != Method::HEAD {
                    return method_not_allowed();
                }
                report(&*shared, &req, id).await
            }
            _ => error(StatusCode::NOT_FOUND, "no such endpoint"),
        },
    }
}

async fn start(shared: &dyn PanelHost, req: &Request, ctx: &AuthContext) -> Response {
    let body = if req.body.is_empty() {
        Value::Null
    } else {
        match json_body(req) {
            Ok(body) => body,
            Err(resp) => return resp,
        }
    };
    let duration = match start_duration(&body) {
        Ok(duration) => duration,
        Err(text) => return error(StatusCode::BAD_REQUEST, text),
    };
    let who = ctx.principal.name();
    let result = shared
        .timings()
        .start(StartOptions {
            duration,
            by: who.clone(),
        })
        .await;
    let entry = Entry::new(who, "timings.start").remote(guard::client_ip(shared, req));
    let entry = match duration {
        Some(after) => entry.detail(format!("{} s", after.as_secs())),
        None => entry,
    };
    match result {
        Ok(recording) => {
            shared.web().audit.record(entry);
            Response::json(
                StatusCode::OK,
                &json!({ "recording": recording_json(&recording) }),
            )
        }
        Err(err) => {
            shared.web().audit.record(entry.outcome(err.to_string()));
            failure(&err)
        }
    }
}

async fn stop(shared: &dyn PanelHost, req: &Request, ctx: &AuthContext) -> Response {
    let entry =
        Entry::new(ctx.principal.name(), "timings.stop").remote(guard::client_ip(shared, req));
    match shared.timings().stop().await {
        Ok(summary) => {
            shared.web().audit.record(entry.target(summary.id.clone()));
            Response::json(
                StatusCode::OK,
                &json!({ "summary": summary_json(&summary) }),
            )
        }
        Err(err) => {
            if !matches!(err, TimingsError::Recorder(RecorderError::NotRecording)) {
                shared.web().audit.record(entry.outcome(err.to_string()));
            }
            failure(&err)
        }
    }
}

async fn report(shared: &dyn PanelHost, req: &Request, id: &str) -> Response {
    let format = match req.query_param("format") {
        None => Export::Json,
        Some(name) => match Export::parse(&name) {
            Some(format) => format,
            None => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "format is json, folded or speedscope",
                );
            }
        },
    };
    let bytes = match shared.timings().export(id, format).await {
        Ok(bytes) => bytes,
        Err(err) => return failure(&err),
    };
    let content_type = match format {
        Export::Folded => "text/plain; charset=utf-8",
        Export::Json | Export::Speedscope => "application/json",
    };
    let mut resp = Response::conditional(req, content_type, bytes);
    if req.query_param("download").is_some_and(|v| v != "0") {
        resp.set_header(
            "content-disposition",
            &format!("attachment; filename=\"{id}.{}\"", format.extension()),
        );
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn start_bodies_parse_the_duration() {
        assert_eq!(start_duration(&Value::Null), Ok(None));
        assert_eq!(start_duration(&json!({})), Ok(None));
        assert_eq!(start_duration(&json!({ "seconds": null })), Ok(None));
        assert_eq!(
            start_duration(&json!({ "seconds": 30 })),
            Ok(Some(Duration::from_secs(30)))
        );
        assert_eq!(
            start_duration(&json!({ "seconds": MAX_SECONDS })),
            Ok(Some(Duration::from_secs(MAX_SECONDS)))
        );
        for bad in [
            json!({ "seconds": 0 }),
            json!({ "seconds": -5 }),
            json!({ "seconds": 1.5 }),
            json!({ "seconds": "10" }),
            json!({ "seconds": MAX_SECONDS + 1 }),
        ] {
            assert!(start_duration(&bad).is_err(), "{bad}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn summaries_and_status_serialize() {
        let summary = Summary {
            id: "timings-1".into(),
            file: Err("no server file area".into()),
            bytes: 10,
            duration: Duration::from_millis(1500),
            ticks: 30,
            tps: Some(20.0),
            mspt_mean: Some(1.5),
            mspt_p99: Some(3.0),
            mspt_max: Some(4.0),
            top: vec![("region.tick [overworld 0,0]".into(), 900)],
            features: vec![("vanilla-mobs".into(), 400)],
            threads: 3,
            dropped: 0,
        };
        let status = Status {
            recording: Some(Recording {
                started_unix_ms: 7,
                elapsed: Duration::from_millis(2500),
                threads: 4,
                stops_after: Some(Duration::from_secs(60)),
                by: "Console".into(),
            }),
            busy_elsewhere: false,
            last: Some(summary),
        };
        let json = status_json(&status);
        assert_eq!(json["recording"]["elapsed_ms"], 2500);
        assert_eq!(json["recording"]["stops_after_s"], 60);
        assert_eq!(json["last"]["file"], Value::Null);
        assert_eq!(json["last"]["file_error"], "no server file area");
        assert_eq!(json["last"]["duration_ms"], 1500);
        assert_eq!(json["last"]["top"][0]["self_ns"], 900);
        assert_eq!(json["last"]["features"][0]["id"], "vanilla-mobs");
        let idle = status_json(&Status {
            recording: None,
            busy_elsewhere: true,
            last: None,
        });
        assert_eq!(idle["recording"], Value::Null);
        assert_eq!(idle["busy_elsewhere"], true);
    }
}
