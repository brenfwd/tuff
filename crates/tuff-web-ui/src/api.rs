//! The panel's routes: the frontend, sessions, the console, logs, the
//! audit log and the event stream.

use std::sync::Arc;

use bytes::Bytes;
use hyper::{Method, StatusCode};
use serde_json::{Value, json};

use super::audit::Entry;
use super::auth::{self, AuthContext, ExchangeError, Scope};
use super::guard::{self, WebGuard, error};
use super::host::{LogRecord, PanelHost, TextFormat};
use super::panel::{APIS_PREFIX, ApiCall, Installed, PAGES_PREFIX};
use tuff_http::{Cookie, Handler, MAIN_LISTENER, Request, Response, RouteOptions, SameSite};

/// Longest command line the console accepts.
pub const MAX_COMMAND: usize = 32 * 1024;
const MAX_JSON_BODY: usize = 64 * 1024;
const MAX_LOG_PAGE: usize = 2000;

pub fn log_json(line: &dyn LogRecord) -> Value {
    json!({
        "seq": line.seq(),
        "ts": line.unix_millis(),
        "level": line.level().to_string(),
        "target": line.target(),
        "text": super::auth::mask_tokens(line.text()),
    })
}

fn route<F, Fut>(shared: &Arc<dyn PanelHost>, f: F) -> Handler
where
    F: Fn(Arc<dyn PanelHost>, Request) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Response> + Send + 'static,
{
    let weak = Arc::downgrade(shared);
    let f = Arc::new(f);
    tuff_http::handler(move |req| {
        let weak = weak.clone();
        let f = f.clone();
        async move {
            match weak.upgrade() {
                Some(shared) => f(shared, req).await,
                None => Response::status(StatusCode::SERVICE_UNAVAILABLE),
            }
        }
    })
}

pub(super) fn require(req: &Request, scope: Option<Scope>) -> Result<Arc<AuthContext>, Response> {
    let ctx = req
        .auth::<AuthContext>()
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "log in first"))?;
    match scope {
        Some(scope) if !ctx.allows(scope) => Err(error(StatusCode::FORBIDDEN, "not allowed")),
        _ => Ok(ctx),
    }
}

pub(super) fn method_not_allowed() -> Response {
    error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")
}

pub(super) fn json_body(req: &Request) -> Result<Value, Response> {
    let typed = req
        .header("content-type")
        .is_some_and(|t| t.trim_start().starts_with("application/json"));
    if !typed {
        return Err(error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "expected application/json",
        ));
    }
    serde_json::from_slice(&req.body).map_err(|_| error(StatusCode::BAD_REQUEST, "bad json"))
}

/// Parses a text field the way the server will (`format` is
/// `minimessage`, `legacy` for `&` codes, `section` for section-sign
/// codes or `mixed`) and returns the component JSON.
fn text_preview(host: &dyn PanelHost, req: &Request) -> Response {
    if let Err(resp) = require(req, None) {
        return resp;
    }
    if req.method != Method::POST {
        return method_not_allowed();
    }
    let body = match json_body(req) {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let Some(text) = body.get("text").and_then(Value::as_str) else {
        return error(StatusCode::BAD_REQUEST, "text is a string");
    };
    let format = match body.get("format").and_then(Value::as_str) {
        None | Some("minimessage") => TextFormat::MiniMessage,
        Some("legacy") => TextFormat::Legacy,
        Some("section") => TextFormat::Section,
        Some("mixed") => TextFormat::Mixed,
        Some(_) => return error(StatusCode::BAD_REQUEST, "unknown format"),
    };
    let (component, plain) = host.text_preview(text, format);
    Response::json(
        StatusCode::OK,
        &json!({ "component": component, "plain": plain }),
    )
}

fn session_json(ctx: &AuthContext) -> Value {
    json!({
        "principal": ctx.principal.to_json(),
        "level": ctx.level.id(),
        "csrf": ctx.csrf,
        "scopes": ctx.scopes(),
    })
}

pub async fn register(shared: &Arc<dyn PanelHost>, panel: Arc<Installed>) {
    let http = shared.http();
    let view = shared.web().live.view();
    let json_opts = RouteOptions {
        max_body: Some(MAX_JSON_BODY),
        stream: false,
    };
    http.set_guard(
        MAIN_LISTENER,
        super::PREFIX,
        Some(Arc::new(WebGuard {
            shared: Arc::downgrade(shared),
        })),
    )
    .await;
    let routes: Vec<(&str, Handler)> = vec![
        ("/admin", route(shared, |_, req| async move { shell(&req) })),
        (
            "/admin/api",
            route(shared, |_, _| async {
                error(StatusCode::NOT_FOUND, "no such endpoint")
            }),
        ),
        (super::SESSION_ROUTE, route(shared, session)),
        (super::COMMAND_ROUTE, route(shared, command)),
        (super::COMPLETE_ROUTE, route(shared, complete)),
        (super::LOGS_ROUTE, route(shared, logs)),
        (super::AUDIT_ROUTE, route(shared, audit)),
        (super::STATUS_ROUTE, route(shared, status)),
        (super::METRICS_ROUTE, {
            let view = view.clone();
            route(shared, move |shared, req| {
                super::dashboard::metrics(shared, req, view.get())
            })
        }),
        (super::TIMINGS_ROUTE, route(shared, super::timings::handle)),
        (
            super::TEXT_PREVIEW_ROUTE,
            route(shared, |s, r| async move { text_preview(&*s, &r) }),
        ),
        (
            super::PERMISSIONS_ROUTE,
            route(shared, super::permissions::handle),
        ),
        (super::PLAYERS_ROUTE, route(shared, super::players::handle)),
        (super::CONFIG_ROUTE, route(shared, super::config::handle)),
        (super::SQL_ROUTE, route(shared, super::sql::handle)),
        (super::TOKENS_ROUTE, route(shared, super::access::tokens)),
        (
            super::SESSIONS_ROUTE,
            route(shared, super::access::sessions),
        ),
        (super::WORLD_ROUTE, route(shared, super::world::handle)),
        (super::EVENTS_ROUTE, {
            let view = view.clone();
            route(shared, move |shared, req| events(shared, req, view.get()))
        }),
        (super::PAGES_ROUTE, {
            let view = view.clone();
            route(shared, move |_, req| {
                let panel = view.get();
                async move { pages(&panel, &req) }
            })
        }),
        (PAGES_PREFIX, {
            let view = view.clone();
            route(shared, move |_, req| {
                let panel = view.get();
                async move { page_asset(&panel, &req) }
            })
        }),
        (APIS_PREFIX, {
            let view = view.clone();
            route(shared, move |shared, req| live_api(shared, req, view.get()))
        }),
    ];
    for (prefix, handler) in routes {
        http.register_route_with(MAIN_LISTENER, prefix, json_opts, handler)
            .await;
    }
    let transfers = super::backups::Transfers::spawn();
    let streaming = RouteOptions {
        max_body: Some(usize::MAX),
        stream: true,
    };
    let uploads: Vec<(&str, Handler)> = vec![
        (super::BACKUPS_ROUTE, {
            let transfers = transfers.clone();
            route(shared, move |shared, req| {
                super::backups::handle(shared, req, transfers.clone())
            })
        }),
        (super::DATAPACKS_ROUTE, {
            let transfers = transfers.clone();
            route(shared, move |shared, req| {
                super::datapacks::handle(shared, req, transfers.clone())
            })
        }),
        (super::RESOURCE_PACKS_ROUTE, {
            let transfers = transfers.clone();
            route(shared, move |shared, req| {
                super::resource_packs::handle(shared, req, transfers.clone())
            })
        }),
    ];
    for (prefix, handler) in uploads {
        http.register_route_with(MAIN_LISTENER, prefix, streaming, handler)
            .await;
    }
    for api in panel.apis.values() {
        let call = api.clone();
        let handler = route(shared, move |shared, request| {
            let api = call.clone();
            async move {
                let auth = match require(&request, Some(api.scope)) {
                    Ok(auth) => auth,
                    Err(resp) => return resp,
                };
                (api.handler)(ApiCall {
                    server: shared.server(),
                    auth,
                    request,
                })
                .await
            }
        });
        http.register_route_with(MAIN_LISTENER, &api.route(), api.options, handler)
            .await;
    }
}

async fn live_api(
    shared: Arc<dyn PanelHost>,
    mut request: Request,
    panel: Arc<Installed>,
) -> Response {
    let rest = request.rest.clone();
    let (id, tail) = match rest.strip_prefix('/') {
        Some(rest) => match rest.find('/') {
            Some(at) => (&rest[..at], &rest[at..]),
            None => (rest, ""),
        },
        None => return error(StatusCode::NOT_FOUND, "no such endpoint"),
    };
    let Some(api) = panel.apis.get(id).cloned() else {
        return error(StatusCode::NOT_FOUND, "no such endpoint");
    };
    let auth = match require(&request, Some(api.scope)) {
        Ok(auth) => auth,
        Err(resp) => return resp,
    };
    request.rest = tail.to_owned();
    (api.handler)(ApiCall {
        server: shared.server(),
        auth,
        request,
    })
    .await
}

fn shell(req: &Request) -> Response {
    if req.rest == "/frame.html" {
        let mut resp = Response::conditional(
            req,
            "text/html; charset=utf-8",
            Bytes::from_static(crate::FRAME_HTML),
        );
        resp.set_header("content-security-policy", &guard::frame_csp());
        return resp;
    }
    if !matches!(req.rest.as_str(), "" | "/" | "/index.html" | "/login") {
        return Response::status(StatusCode::NOT_FOUND);
    }
    let mut resp = Response::conditional(
        req,
        "text/html; charset=utf-8",
        Bytes::from_static(crate::INDEX_HTML),
    );
    resp.set_header("content-security-policy", &guard::shell_csp());
    resp
}

fn pages(panel: &Installed, req: &Request) -> Response {
    let ctx = match require(req, None) {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    if req.method != Method::GET {
        return method_not_allowed();
    }
    let list: Vec<Value> = panel
        .visible(&ctx)
        .into_iter()
        .map(|p| {
            json!({
                "id": p.id,
                "title": p.title,
                "section": p.section,
                "icon": p.icon,
                "scope": p.scope.name(),
                "module": p.module_url(),
                "styles": p.styles.iter().map(|s| p.asset_url(s)).collect::<Vec<_>>(),
            })
        })
        .collect();
    Response::json(StatusCode::OK, &json!({ "pages": list }))
}

fn page_asset(panel: &Installed, req: &Request) -> Response {
    let ctx = match require(req, None) {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    if req.method != Method::GET && req.method != Method::HEAD {
        return method_not_allowed();
    }
    let Some((id, path)) = req
        .rest
        .strip_prefix('/')
        .and_then(|rest| rest.split_once('/'))
    else {
        return error(StatusCode::NOT_FOUND, "no such page");
    };
    let Some(page) = panel.pages.get(id) else {
        return error(StatusCode::NOT_FOUND, "no such page");
    };
    if let Err(resp) = ctx.require(page.scope) {
        return resp;
    }
    match page.asset_at(path) {
        Some(asset) => Response::conditional(req, &asset.content_type, asset.bytes.clone()),
        None => error(StatusCode::NOT_FOUND, "no such asset"),
    }
}

async fn session(shared: Arc<dyn PanelHost>, req: Request) -> Response {
    if !req.rest.is_empty() {
        return error(StatusCode::NOT_FOUND, "no such endpoint");
    }
    match req.method {
        Method::GET => match require(&req, None) {
            Ok(ctx) => Response::json(StatusCode::OK, &session_json(&ctx)),
            Err(resp) => resp,
        },
        Method::POST => login(&*shared, &req).await,
        Method::DELETE => {
            let ctx = match require(&req, None) {
                Ok(ctx) => ctx,
                Err(resp) => return resp,
            };
            shared.web().auth.logout(&ctx.session).await;
            shared.web().audit.record(
                Entry::new(ctx.principal.name(), "logout").remote(guard::client_ip(&*shared, &req)),
            );
            removal_cookie(Response::json(StatusCode::OK, &json!({})))
        }
        _ => method_not_allowed(),
    }
}

fn removal_cookie(resp: Response) -> Response {
    let cookie = Cookie::removal(auth::COOKIE)
        .path(super::PREFIX)
        .http_only()
        .same_site(SameSite::Strict);
    match resp.set_cookie(&cookie) {
        Ok(resp) => resp,
        Err(_) => Response::status(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn login(shared: &dyn PanelHost, req: &Request) -> Response {
    let ip = guard::client_ip(shared, req);
    let body = match json_body(req) {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let code = body.get("code").and_then(Value::as_str).unwrap_or_default();
    match shared.web().auth.exchange(code, ip).await {
        Ok(new) => {
            let level = guard::level_of(shared, &new.principal).await;
            let permissions = shared.permissions();
            if !auth::may_use(
                &*permissions,
                &new.principal,
                level,
                guard::min_level(shared),
            ) {
                shared.web().auth.logout(&new.session).await;
                shared.web().audit.record(
                    Entry::new(new.principal.name(), "login")
                        .remote(ip)
                        .outcome("not allowed"),
                );
                return error(StatusCode::FORBIDDEN, "you may not use the panel");
            }
            shared
                .web()
                .audit
                .record(Entry::new(new.principal.name(), "login").remote(ip));
            let ctx = AuthContext {
                principal: new.principal,
                level,
                session: new.session,
                csrf: new.csrf,
                permissions,
            };
            let cookie = Cookie::new(auth::COOKIE, new.id)
                .path(super::PREFIX)
                .http_only()
                .same_site(SameSite::Strict);
            Response::json(StatusCode::OK, &session_json(&ctx))
                .set_cookie(&cookie)
                .unwrap_or_else(|_| Response::status(StatusCode::INTERNAL_SERVER_ERROR))
        }
        Err(ExchangeError::RateLimited(wait)) => {
            shared.web().audit.record(
                Entry::new("anonymous", "login")
                    .remote(ip)
                    .outcome("rate limited"),
            );
            error(
                StatusCode::TOO_MANY_REQUESTS,
                "too many attempts, wait and retry",
            )
            .header("retry-after", &wait.as_secs().max(1).to_string())
        }
        Err(ExchangeError::Invalid) => {
            shared.web().audit.record(
                Entry::new("anonymous", "login")
                    .remote(ip)
                    .outcome("bad code"),
            );
            error(StatusCode::UNAUTHORIZED, "unknown or expired code")
        }
    }
}

/// Refuses control characters and oversized lines.
pub fn valid_command(line: &str) -> bool {
    !line.trim().is_empty() && line.len() <= MAX_COMMAND && !line.chars().any(char::is_control)
}

async fn command(shared: Arc<dyn PanelHost>, req: Request) -> Response {
    let ctx = match require(&req, Some(Scope::ConsoleRun)) {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    if req.method != Method::POST {
        return method_not_allowed();
    }
    let body = match json_body(&req) {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let line = body.get("line").and_then(Value::as_str).unwrap_or_default();
    if !valid_command(line) {
        return error(
            StatusCode::BAD_REQUEST,
            "empty, oversized or control characters",
        );
    }
    if !shared.web().auth.command_allowed(&ctx.session).await {
        return error(StatusCode::TOO_MANY_REQUESTS, "slow down");
    }
    let line = line.trim().trim_start_matches('/').to_owned();
    let who = ctx.principal.name();
    let shown = super::audit::redact_with(&line, |key| shared.config_secret(key));
    log::info!(target: "tuff::web", "{who} issued server command: /{shown}");
    let (feedback, _) =
        super::run_command(shared.clone(), ctx.principal.clone(), ctx.level, &line).await;
    for text in &feedback {
        log::info!(target: "tuff::web", "[{who}] {text}");
    }
    shared.web().audit.record(
        Entry::new(who, "command")
            .remote(guard::client_ip(&*shared, &req))
            .target(shown),
    );
    Response::json(StatusCode::OK, &json!({ "feedback": feedback }))
}

async fn complete(shared: Arc<dyn PanelHost>, req: Request) -> Response {
    let ctx = match require(&req, Some(Scope::ConsoleRun)) {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    let line = req.query_param("line").unwrap_or_default();
    if line.len() > MAX_COMMAND || line.chars().any(char::is_control) {
        return error(StatusCode::BAD_REQUEST, "bad line");
    }
    let slashed = line.starts_with('/');
    let found = shared
        .complete(line.clone(), ctx.principal.clone(), ctx.level)
        .await;
    let start = if slashed {
        found.start
    } else {
        found.start.saturating_sub(1)
    };
    Response::json(
        StatusCode::OK,
        &json!({ "start": start, "length": found.length, "matches": found.matches }),
    )
}

async fn logs(shared: Arc<dyn PanelHost>, req: Request) -> Response {
    if let Err(resp) = require(&req, Some(Scope::LogsRead)) {
        return resp;
    }
    let limit = req
        .query_param("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(500)
        .clamp(1, MAX_LOG_PAGE);
    let before = req
        .query_param("before")
        .and_then(|v| v.parse::<u64>().ok());
    let level = req
        .query_param("level")
        .and_then(|v| v.parse::<log::Level>().ok())
        .unwrap_or(log::Level::Trace);
    let backfill = shared.log_backfill().await;
    let lines = log_page(&backfill, before, limit, level);
    Response::json(StatusCode::OK, &json!({ "lines": lines }))
}

/// The newest `limit` lines at or above `level` before sequence `before`,
/// oldest first.
pub fn log_page(
    lines: &[Arc<dyn LogRecord>],
    before: Option<u64>,
    limit: usize,
    level: log::Level,
) -> Vec<Value> {
    let mut out: Vec<Value> = lines
        .iter()
        .rev()
        .filter(|l| l.level() <= level && before.is_none_or(|b| l.seq() < b))
        .take(limit)
        .map(|l| log_json(&**l))
        .collect();
    out.reverse();
    out
}

async fn audit(shared: Arc<dyn PanelHost>, req: Request) -> Response {
    if let Err(resp) = require(&req, Some(Scope::PanelAdmin)) {
        return resp;
    }
    let limit = req
        .query_param("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(100);
    let before = req
        .query_param("before")
        .and_then(|v| v.parse::<u64>().ok());
    let text = |name: &str| req.query_param(name).filter(|v| !v.trim().is_empty());
    let filter = super::audit::Filter {
        principal: text("principal"),
        action: text("action"),
    };
    let entries = shared
        .web()
        .audit
        .page_filtered(before, limit, filter)
        .await;
    Response::json(StatusCode::OK, &json!({ "entries": entries }))
}

async fn status(shared: Arc<dyn PanelHost>, req: Request) -> Response {
    if let Err(resp) = require(&req, None) {
        return resp;
    }
    Response::json(
        StatusCode::OK,
        &json!({
            "auditFailures": shared.web().audit.failures(),
            "sessions": shared.web().auth.session_count().await,
        }),
    )
}

async fn events(shared: Arc<dyn PanelHost>, req: Request, panel: Arc<Installed>) -> Response {
    let ctx = match require(&req, None) {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    let topics = req.query_param("topics").unwrap_or_default();
    let after = req
        .header("last-event-id")
        .and_then(|v| v.trim().parse::<u64>().ok());
    let plugin_topic = |topic: &str| panel.topic_allowed(&ctx, topic);
    match shared
        .web()
        .events
        .stream_with(&ctx, &topics, after, plugin_topic)
        .await
    {
        Ok(rx) => Response::event_stream(rx),
        Err(_) => error(StatusCode::TOO_MANY_REQUESTS, "too many event streams"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn log_pages_filter_level_and_position() {
        struct Line(u64, log::Level);
        impl LogRecord for Line {
            fn seq(&self) -> u64 {
                self.0
            }
            fn unix_millis(&self) -> i64 {
                0
            }
            fn level(&self) -> log::Level {
                self.1
            }
            fn target(&self) -> &str {
                "t"
            }
            fn text(&self) -> &str {
                "line"
            }
        }
        let lines: Vec<Arc<dyn LogRecord>> = (0..10u64)
            .map(|seq| {
                let level = if seq % 2 == 0 {
                    log::Level::Info
                } else {
                    log::Level::Debug
                };
                Arc::new(Line(seq, level)) as Arc<dyn LogRecord>
            })
            .collect();
        let page = log_page(&lines, Some(8), 2, log::Level::Info);
        let seqs: Vec<u64> = page.iter().map(|v| v["seq"].as_u64().unwrap()).collect();
        assert_eq!(seqs, [4, 6]);
        assert_eq!(log_page(&lines, None, 100, log::Level::Trace).len(), 10);
        assert!(valid_command("say hi"));
        assert!(!valid_command("say\u{7}hi"));
        assert!(!valid_command("   "));
        assert!(!valid_command(&"a".repeat(MAX_COMMAND + 1)));
    }
}
