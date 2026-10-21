//! The map's panel API under `/admin/api/v1/plugins/world-map`:
//!
//! - `GET /`: tile geometry, the ETag epoch, the dimensions and the spawn;
//! - `GET /tiles/<dim>/<level>/<x>/<z>.png`: a tile, `ETag`ged per tile
//!   version; `If-None-Match` with the current tag answers 304 without
//!   rendering. 429 when the session's tile budget is spent, 503 when the
//!   render queue is full;
//! - `POST /viewport` `{dim, level, minX, minZ, maxX, maxZ}`: what the page
//!   shows, held for 15 s; live events are published only while one is;
//! - `GET /markers`: the last player marker snapshot;
//! - `GET /pregen`: the chunk pre-generation status;
//! - `POST /pregen` `{dim, x, z, radius, shape, regenerate, confirm}`:
//!   starts a job (as `/tuff chunk generate`); regenerating needs
//!   `confirm: "regenerate"`;
//! - `POST /pregen/cancel`: cancels the running job.
//!
//! The API's scope is `map.view`; the panel guard checks it before any
//! handler runs. Starting and cancelling jobs also needs `map.generate`.

use serde_json::{Value, json};
use tuff_http::Response;
use tuff_web_ui::panel::{ApiCall, Method, StatusCode};

use tuff_core::world::pregen::{Area, JobSpec, MAX_RADIUS, PregenError, Shape};

use crate::service::{MapHandle, MapService, TileReply, Viewport};
use crate::tile::{MAX_LEVEL, SIDE, TileKey};

fn error(status: StatusCode, message: &str) -> Response {
    Response::json(status, &json!({ "error": message }))
}

/// Parses `/<dim>/<level>/<x>/<z>.png`.
pub fn parse_tile(path: &str) -> Option<TileKey> {
    let rest = path.strip_prefix('/')?.strip_suffix(".png")?;
    let mut parts = rest.split('/');
    let dim = parts.next()?.parse().ok()?;
    let level = parts.next()?.parse().ok()?;
    let x = parts.next()?.parse().ok()?;
    let z = parts.next()?.parse().ok()?;
    if parts.next().is_some() || level > MAX_LEVEL {
        return None;
    }
    Some(TileKey::new(dim, level, x, z))
}

pub async fn handle(call: ApiCall) -> Response {
    let Some(shared) = call.server::<tuff_core::world::Shared>() else {
        return Response::status(StatusCode::SERVICE_UNAVAILABLE);
    };
    let ApiCall { auth, request, .. } = call;
    let Some(map) = shared
        .services
        .get::<MapService>()
        .and_then(MapService::handle)
    else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "the map is not running");
    };
    let client = if auth.session.is_empty() {
        auth.principal.name()
    } else {
        auth.session.clone()
    };
    let rest = request.rest.as_str();
    match (&request.method, rest) {
        (&Method::GET, "" | "/") => Response::json(StatusCode::OK, &info(&shared, map)),
        (&Method::GET, "/markers") => {
            Response::new(StatusCode::OK, map.markers().await.to_string())
                .header("content-type", "application/json")
        }
        (&Method::POST, "/viewport") => match viewport(&request.body) {
            Some(view) => {
                map.viewport(&client, view).await;
                Response::json(StatusCode::OK, &json!({ "ok": true }))
            }
            None => error(StatusCode::BAD_REQUEST, "bad viewport"),
        },
        (&Method::GET, "/pregen") => Response::json(StatusCode::OK, &json!(shared.pregen.status())),
        (&Method::POST, "/pregen" | "/pregen/cancel") => {
            if let Err(denied) = auth.require(crate::GENERATE) {
                return denied;
            }
            let result = if rest == "/pregen" {
                let Some(spec) = job(&shared, &request.body, auth.principal.name()) else {
                    return error(StatusCode::BAD_REQUEST, "bad job");
                };
                shared.pregen.start(&shared, spec).await
            } else {
                shared.pregen.cancel().await
            };
            match result {
                Ok(progress) => Response::json(StatusCode::OK, &json!(progress)),
                Err(err @ (PregenError::Busy(_) | PregenError::NotRunning)) => {
                    error(StatusCode::CONFLICT, &err.to_string())
                }
                Err(err) => error(StatusCode::SERVICE_UNAVAILABLE, &err.to_string()),
            }
        }
        (&Method::GET, path) if path.starts_with("/tiles/") => {
            let Some(key) = parse_tile(&path["/tiles".len()..]) else {
                return error(StatusCode::NOT_FOUND, "no such tile");
            };
            let tags = request.header("if-none-match").map(str::to_owned);
            tile_response(map.tile(key, &client, tags).await)
        }
        _ => error(StatusCode::NOT_FOUND, "no such endpoint"),
    }
}

fn tile_response(reply: TileReply) -> Response {
    match reply {
        TileReply::Png { etag, png } => Response::new(StatusCode::OK, png)
            .header("content-type", "image/png")
            .header("etag", &etag),
        TileReply::NotModified { etag } => {
            Response::status(StatusCode::NOT_MODIFIED).header("etag", &etag)
        }
        TileReply::Limited(after) => error(StatusCode::TOO_MANY_REQUESTS, "too many tiles")
            .header("retry-after", &after.to_string()),
        TileReply::Busy => {
            error(StatusCode::SERVICE_UNAVAILABLE, "the map is busy").header("retry-after", "1")
        }
        TileReply::NotFound => error(StatusCode::NOT_FOUND, "no such tile"),
        TileReply::Failed => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the tile failed to render",
        ),
    }
}

/// A pre-generation job from the page's JSON; a regenerating one must
/// say `confirm: "regenerate"`.
pub fn job(shared: &tuff_core::world::Shared, body: &[u8], by: String) -> Option<JobSpec> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let int = |name: &str| -> Option<i32> { i32::try_from(value.get(name)?.as_i64()?).ok() };
    let dim = usize::try_from(value.get("dim")?.as_u64()?).ok()?;
    let dimension = shared.dimensions.iter().nth(dim)?.id;
    let shape = Shape::parse(value.get("shape")?.as_str()?)?;
    let radius = int("radius")?;
    let regenerate = value
        .get("regenerate")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !(0..=MAX_RADIUS).contains(&radius)
        || (regenerate && value.get("confirm").and_then(Value::as_str) != Some("regenerate"))
    {
        return None;
    }
    Some(JobSpec {
        dimension,
        area: Area::new(int("x")?, int("z")?, radius, shape),
        regenerate,
        by,
    })
}

fn viewport(body: &[u8]) -> Option<Viewport> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let int = |name: &str| -> Option<i32> { i32::try_from(value.get(name)?.as_i64()?).ok() };
    let dim = u16::try_from(value.get("dim")?.as_u64()?).ok()?;
    let level = u8::try_from(value.get("level")?.as_u64()?).ok()?;
    let (min_x, min_z, max_x, max_z) = (int("minX")?, int("minZ")?, int("maxX")?, int("maxZ")?);
    (min_x <= max_x && min_z <= max_z && level <= MAX_LEVEL).then_some(Viewport {
        dim,
        level,
        min: [min_x, min_z],
        max: [max_x, max_z],
    })
}

fn info(shared: &tuff_core::world::Shared, map: &MapHandle) -> Value {
    let spawn = shared.spawn.borrow().clone();
    let dimensions: Vec<Value> = shared
        .dimensions
        .iter()
        .enumerate()
        .map(|(index, d)| {
            json!({
                "index": index,
                "key": d.key,
                "minY": d.shape.min_y,
                "height": d.shape.height,
                "ceiling": d.kind.has_ceiling,
            })
        })
        .collect();
    let spawn_dim = shared
        .dimensions
        .iter()
        .position(|d| d.key == spawn.dimension)
        .unwrap_or(0);
    json!({
        "epoch": format!("{:08x}", map.epoch()),
        "tileSize": SIDE,
        "maxLevel": MAX_LEVEL,
        "dimensions": dimensions,
        "spawn": { "dim": spawn_dim, "x": spawn.pos.x, "z": spawn.pos.z },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn tile_paths_parse_strictly() {
        assert_eq!(parse_tile("/0/0/-1/2.png"), Some(TileKey::new(0, 0, -1, 2)));
        assert_eq!(parse_tile("/2/6/0/0.png"), Some(TileKey::new(2, 6, 0, 0)));
        for bad in [
            "/0/7/0/0.png",
            "/0/0/0.png",
            "/0/0/0/0",
            "/0/0/0/0/0.png",
            "/x/0/0/0.png",
            "0/0/0/0.png",
        ] {
            assert_eq!(parse_tile(bad), None, "{bad}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn viewports_are_validated() {
        let ok = br#"{"dim":0,"level":2,"minX":-10,"minZ":-5,"maxX":10,"maxZ":5}"#;
        assert_eq!(
            viewport(ok),
            Some(Viewport {
                dim: 0,
                level: 2,
                min: [-10, -5],
                max: [10, 5]
            })
        );
        assert_eq!(
            viewport(br#"{"dim":0,"level":9,"minX":0,"minZ":0,"maxX":1,"maxZ":1}"#),
            None
        );
        assert_eq!(
            viewport(br#"{"dim":0,"level":0,"minX":5,"minZ":0,"maxX":1,"maxZ":1}"#),
            None
        );
        assert_eq!(viewport(b"nope"), None);
    }
}
