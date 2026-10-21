//! Runs before every request under `/admin`: the on switch, the remote
//! and Host checks, session authentication, the live operator level,
//! CSRF, and the security headers on every response.

use std::net::IpAddr;
use std::sync::{Arc, Weak};

use hyper::{Method, StatusCode};
use serde_json::json;

use super::auth::{self, AuthContext, Authenticated, Principal};
use super::host::PanelHost;
use tuff_http::{GuardFuture, Request, Response};
use tuff_storage::admin::PermissionLevel;

pub const CSRF_HEADER: &str = "x-tuff-csrf";

pub struct WebGuard {
    pub shared: Weak<dyn PanelHost>,
}

pub fn error(status: StatusCode, message: &str) -> Response {
    Response::json(status, &json!({ "error": message }))
}

pub const FRAME_PATH: &str = "/admin/frame.html";

/// Paths reachable without a session: the frontends and the code exchange.
fn public(method: &Method, path: &str) -> bool {
    let shell = matches!(
        path,
        "/admin" | "/admin/" | "/admin/index.html" | "/admin/login" | FRAME_PATH
    );
    (shell && (method == Method::GET || method == Method::HEAD))
        || (path == super::SESSION_ROUTE && (method == Method::POST || method == Method::GET))
}

/// The client address, honouring `X-Forwarded-For` only when trusted.
pub fn client_ip(shared: &dyn PanelHost, req: &Request) -> IpAddr {
    if shared.config_bool(super::TRUST_FORWARDED).unwrap_or(false)
        && let Some(ip) = req
            .header("x-forwarded-for")
            .and_then(|v| v.split(',').next())
            .and_then(|v| v.trim().parse().ok())
    {
        return ip;
    }
    req.remote.ip()
}

fn host_of(shared: &dyn PanelHost, req: &Request) -> Option<String> {
    let host = if shared.config_bool(super::TRUST_FORWARDED).unwrap_or(false) {
        req.header("x-forwarded-host")
            .map(|h| h.split(',').next().unwrap_or(h).trim().to_owned())
            .or_else(|| req.host.clone())
    } else {
        req.host.clone()
    };
    host.map(|h| h.to_ascii_lowercase())
}

/// The host without its port; IPv6 literals keep their brackets off.
fn host_name(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    host.rsplit_once(':')
        .filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(host, |(name, _)| name)
}

/// Whether `host` (a `Host` header value) may reach the panel: literal
/// addresses, `localhost`, and configured names. A name pointing at the
/// server from some other site (DNS rebinding) is refused.
pub fn host_allowed(shared: &dyn PanelHost, host: &str) -> bool {
    let name = host_name(host);
    if name.parse::<IpAddr>().is_ok() || name == "localhost" {
        return true;
    }
    let configured = shared.config_string(super::HOSTNAMES).unwrap_or_default();
    if configured
        .split([',', ' '])
        .any(|n| !n.is_empty() && n.eq_ignore_ascii_case(name))
    {
        return true;
    }
    let public = shared.config_string(super::PUBLIC_URL).unwrap_or_default();
    public
        .split_once("://")
        .map(|(_, rest)| rest.split('/').next().unwrap_or(rest))
        .is_some_and(|authority| host_name(authority).eq_ignore_ascii_case(name))
}

/// Whether `name` (already stripped of its port) matches a bound listener
/// address, `localhost`, or a configured hostname.
fn matches_known_host(name: &str, bound_ips: &[IpAddr], hostnames: &str) -> bool {
    if name == "localhost" {
        return true;
    }
    if let Ok(ip) = name.parse::<IpAddr>()
        && bound_ips.contains(&ip)
    {
        return true;
    }
    hostnames
        .split([',', ' '])
        .any(|n| !n.is_empty() && n.eq_ignore_ascii_case(name))
}

/// Pure decision for [`warn_on_proxied_host_mismatch`]: true when at
/// least one of `host`/`forwarded_host` is present and none of the
/// present ones match a bound address or a configured hostname.
fn host_mismatch(
    host: Option<&str>,
    forwarded_host: Option<&str>,
    bound_ips: &[IpAddr],
    hostnames: &str,
) -> bool {
    let mut saw_any = false;
    for candidate in [host, forwarded_host].into_iter().flatten() {
        saw_any = true;
        if matches_known_host(host_name(candidate), bound_ips, hostnames) {
            return false;
        }
    }
    saw_any
}

/// Logs a one-time hint when a proxy rewrote the panel's Host (or sent
/// `X-Forwarded-Host`) to something that is neither a bound listener
/// address nor a configured hostname, while `tuff.web.publicUrl` is
/// unset and `tuff.web.trustForwarded` is false: in that setup plugin
/// pages will not load, because the CSP source is built from the raw
/// `Host` header rather than the proxy's public address.
pub async fn warn_on_proxied_host_mismatch(shared: &dyn PanelHost, req: &Request) {
    if shared
        .web()
        .host_mismatch_warned
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return;
    }
    if !shared
        .config_string(super::PUBLIC_URL)
        .unwrap_or_default()
        .is_empty()
        || shared.config_bool(super::TRUST_FORWARDED).unwrap_or(false)
    {
        return;
    }
    let bound_ips: Vec<IpAddr> = shared
        .listener_addrs()
        .await
        .iter()
        .map(|a| a.ip())
        .collect();
    let hostnames = shared.config_string(super::HOSTNAMES).unwrap_or_default();
    if !host_mismatch(
        req.host.as_deref(),
        req.header("x-forwarded-host"),
        &bound_ips,
        &hostnames,
    ) {
        return;
    }
    if shared
        .web()
        .host_mismatch_warned
        .swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        return;
    }
    log::warn!(
        target: "tuff::web",
        "web panel: request Host/X-Forwarded-Host does not match a bound address or \
         tuff.web.hostnames; if this server sits behind a reverse proxy, set \
         tuff.web.publicUrl (and tuff.web.trustForwarded if the proxy sets \
         X-Forwarded-Host/-For) so plugin pages load correctly"
    );
}

/// An `Origin` (or `Referer`) from another site than the request's host.
fn cross_origin(req: &Request, host: &str) -> bool {
    let origin = req
        .header("origin")
        .filter(|o| *o != "null")
        .or_else(|| req.header("referer"));
    let Some(origin) = origin else {
        return req.header("origin") == Some("null");
    };
    let authority = origin
        .split_once("://")
        .map(|(_, rest)| rest.split('/').next().unwrap_or(rest));
    authority.is_none_or(|a| !a.eq_ignore_ascii_case(host))
}

/// The policy of every panel response: only the shell's own inline script
/// and style. Plugin page code never runs under it.
pub fn csp() -> String {
    policy("")
}

/// The shell's policy: the base policy plus same-origin frames, which host
/// the sandboxed plugin pages (see [`frame_csp`]).
pub fn shell_csp() -> String {
    policy("frame-src 'self'; ")
}

fn policy(extra: &str) -> String {
    let scripts = crate::SCRIPT_HASHES.join(" ");
    let styles = crate::STYLE_HASHES.join(" ");
    format!(
        "default-src 'none'; script-src {scripts}; style-src {styles}; \
         img-src 'self' data:; connect-src 'self'; font-src 'self'; base-uri 'none'; \
         form-action 'self'; {extra}frame-ancestors 'none'"
    )
}

/// The policy of the plugin page frame document: an opaque-origin sandbox
/// that can run its own inline runtime and the plugin module handed to it
/// as a blob, and can reach nothing over the network.
pub fn frame_csp() -> String {
    let scripts = crate::FRAME_SCRIPT_HASHES.join(" ");
    let styles = crate::FRAME_STYLE_HASHES.join(" ");
    format!(
        "sandbox allow-scripts; default-src 'none'; script-src {scripts} blob:; \
         style-src {styles}; img-src data: blob:; font-src data:; connect-src 'none'; \
         base-uri 'none'; form-action 'none'; frame-ancestors 'self'"
    )
}

/// The live level of `principal`; owners for the console.
pub async fn level_of(shared: &dyn PanelHost, principal: &Principal) -> PermissionLevel {
    match principal {
        Principal::Console => PermissionLevel::Owners,
        Principal::Token { level, .. } => *level,
        Principal::Operator { uuid, .. } => shared.operator_level(*uuid).await,
    }
}

/// Audits a refused bearer token, unless refusals from `ip` are being
/// throttled.
fn audit_refusal(shared: &dyn PanelHost, ip: IpAddr, path: &str, refused: auth::TokenRefusal) {
    let Some(since) = refused.audit else {
        return;
    };
    let (principal, target) = match &refused.token {
        Some(token @ Principal::Token { id, .. }) => (token.name(), id.clone()),
        _ => ("anonymous".to_owned(), String::new()),
    };
    let mut entry = super::audit::Entry::new(principal, "token.use")
        .remote(ip)
        .target(target)
        .outcome(refused.reason);
    entry = if since > 0 {
        entry.detail(format!(
            "{path} ({since} more refused since the last entry)"
        ))
    } else {
        entry.detail(path)
    };
    shared.web().audit.record(entry);
}

pub fn min_level(shared: &dyn PanelHost) -> PermissionLevel {
    shared
        .config_int(super::MIN_OP_LEVEL)
        .and_then(PermissionLevel::from_id)
        .unwrap_or(PermissionLevel::Owners)
}

impl tuff_http::Guard for WebGuard {
    fn check<'a>(&'a self, req: &'a Request) -> GuardFuture<'a> {
        Box::pin(async move {
            let Some(shared) = self.shared.upgrade() else {
                return Err(Response::status(StatusCode::NOT_FOUND));
            };
            if !shared.config_bool(super::ENABLED).unwrap_or(false) {
                return Err(Response::status(StatusCode::NOT_FOUND));
            }
            let ip = client_ip(&*shared, req);
            if !ip.is_loopback() && !super::remote_permitted(&*shared) {
                return Err(error(
                    StatusCode::FORBIDDEN,
                    "remote access to the panel is off",
                ));
            }
            let Some(host) = host_of(&*shared, req) else {
                return Err(error(StatusCode::MISDIRECTED_REQUEST, "missing host"));
            };
            if !host_allowed(&*shared, &host) {
                return Err(error(StatusCode::MISDIRECTED_REQUEST, "unknown host"));
            }
            warn_on_proxied_host_mismatch(&*shared, req).await;
            let mutating = req.method != Method::GET && req.method != Method::HEAD;
            if mutating
                && (cross_origin(req, &host) || req.header("sec-fetch-site") == Some("cross-site"))
            {
                return Err(error(StatusCode::FORBIDDEN, "cross-site request"));
            }
            let is_public = public(&req.method, &req.path);
            let cookie = req.cookie(auth::COOKIE).map(str::to_owned);
            let bearer = req
                .header("authorization")
                .and_then(|h| h.strip_prefix("Bearer "))
                .map(str::to_owned);
            let found = match (&bearer, cookie) {
                (Some(token), _) => shared.web().auth.authenticate_token(token, ip).await,
                (None, Some(id)) => shared.web().auth.authenticate(&id).await,
                (None, None) => Authenticated::Unknown,
            };
            let (principal, session, csrf) = match found {
                Authenticated::Ok {
                    principal,
                    session,
                    csrf,
                } => (principal, session, csrf),
                Authenticated::RateLimited => {
                    return Err(error(StatusCode::TOO_MANY_REQUESTS, "slow down"));
                }
                Authenticated::Refused(refused) => {
                    audit_refusal(&*shared, ip, &req.path, refused);
                    if is_public {
                        return Ok(None);
                    }
                    return Err(error(StatusCode::UNAUTHORIZED, "bad or expired token"));
                }
                Authenticated::Unknown if is_public => return Ok(None),
                Authenticated::Unknown => {
                    return Err(error(StatusCode::UNAUTHORIZED, "log in first"));
                }
            };
            let level = level_of(&*shared, &principal).await;
            let permissions = shared.permissions();
            if !auth::may_use(&*permissions, &principal, level, min_level(&*shared)) {
                shared.web().auth.logout(&session).await;
                if is_public {
                    return Ok(None);
                }
                return Err(error(StatusCode::UNAUTHORIZED, "no longer allowed"));
            }
            if mutating
                && bearer.is_none()
                && !req
                    .header(CSRF_HEADER)
                    .is_some_and(|token| auth::constant_eq(token, &csrf))
            {
                return Err(error(StatusCode::FORBIDDEN, "missing or wrong CSRF token"));
            }
            let ctx: Arc<AuthContext> = Arc::new(AuthContext {
                principal,
                level,
                session,
                csrf,
                permissions,
            });
            Ok(Some(ctx as tuff_http::AuthContext))
        })
    }

    fn finish(&self, _method: &Method, path: &str, resp: &mut Response) {
        let frame = path == FRAME_PATH;
        if resp.header_value("content-security-policy").is_none() {
            resp.set_header("content-security-policy", &csp());
        }
        resp.set_header("x-content-type-options", "nosniff");
        resp.set_header("referrer-policy", "no-referrer");
        resp.set_header("x-frame-options", if frame { "SAMEORIGIN" } else { "DENY" });
        resp.set_header("cross-origin-opener-policy", "same-origin");
        resp.set_header("cross-origin-resource-policy", "same-origin");
        if path.starts_with("/admin/api") || path.starts_with("/admin/events") {
            resp.set_header("cache-control", "no-store");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn host_names_drop_ports() {
        assert_eq!(host_name("example.org:8080"), "example.org");
        assert_eq!(host_name("[::1]:80"), "::1");
        assert_eq!(host_name("127.0.0.1"), "127.0.0.1");
        assert!(public(&Method::GET, "/admin/"));
        assert!(!public(&Method::POST, "/admin/"));
        assert!(public(&Method::POST, "/admin/api/v1/session"));
        assert!(!public(&Method::DELETE, "/admin/api/v1/session"));
        assert!(!public(&Method::GET, "/admin/api/v1/logs"));
    }

    fn blocks<'a>(html: &'a str, tag: &str) -> Vec<&'a str> {
        let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
        let mut out = Vec::new();
        let mut rest = html;
        while let Some(start) = rest.find(&open) {
            let after = &rest[start..];
            let body = &after[after.find('>').unwrap() + 1..];
            let end = body.find(&close).unwrap();
            out.push(&body[..end]);
            rest = &body[end + close.len()..];
        }
        out
    }

    fn hash(block: &str) -> String {
        use base64::Engine;
        use sha2::{Digest, Sha256};
        format!(
            "'sha256-{}'",
            base64::engine::general_purpose::STANDARD.encode(Sha256::digest(block.as_bytes()))
        )
    }

    #[cfg(feature = "panel")]
    #[tuff_core::watchdog]
    #[test]
    fn the_embedded_page_matches_its_hashes_and_loads_nothing_else() {
        let html = std::str::from_utf8(crate::INDEX_HTML).unwrap();
        let scripts: Vec<String> = blocks(html, "script").into_iter().map(hash).collect();
        let styles: Vec<String> = blocks(html, "style").into_iter().map(hash).collect();
        assert!(!scripts.is_empty());
        assert_eq!(scripts, crate::SCRIPT_HASHES);
        assert_eq!(styles, crate::STYLE_HASHES);
        let mut markup = html.to_owned();
        for block in blocks(html, "script")
            .into_iter()
            .chain(blocks(html, "style"))
        {
            markup = markup.replacen(block, "", 1);
        }
        let lower = markup.to_ascii_lowercase();
        assert!(!lower.contains(" src="), "external resource in {markup}");
        assert!(!lower.contains("<link"), "{markup}");
        assert!(!lower.contains(" style="), "{markup}");
        for word in lower.split(|c: char| c.is_whitespace() || c == '<') {
            assert!(
                !(word.starts_with("on") && word.contains('=')),
                "inline handler {word} in {markup}"
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_frame_policy_is_sandboxed_and_networkless() {
        let policy = frame_csp();
        assert!(policy.starts_with("sandbox allow-scripts;"));
        assert!(!policy.contains("allow-same-origin"));
        assert!(policy.contains("connect-src 'none'"));
        assert!(!policy.contains("unsafe"));
        assert!(!policy.contains("'self'; "), "{policy}");
        assert!(shell_csp().contains("frame-src 'self'"));
        assert!(!csp().contains("frame-src"));
    }

    #[cfg(feature = "panel")]
    #[tuff_core::watchdog]
    #[test]
    fn csp_carries_the_build_hashes_and_no_unsafe_inline() {
        let policy = csp();
        assert!(!policy.contains("unsafe"));
        for hash in crate::SCRIPT_HASHES {
            assert!(policy.contains(hash));
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn proxied_host_mismatch_is_detected_against_bound_ips_and_hostnames() {
        let bound: Vec<IpAddr> = vec!["127.0.0.1".parse().unwrap()];
        // Host matches a bound address.
        assert!(!host_mismatch(Some("127.0.0.1:8080"), None, &bound, ""));
        // Host matches a configured hostname.
        assert!(!host_mismatch(
            Some("panel.example.org"),
            None,
            &bound,
            "panel.example.org"
        ));
        // Host is bare localhost.
        assert!(!host_mismatch(Some("localhost:80"), None, &bound, ""));
        // Neither header is present: nothing to flag.
        assert!(!host_mismatch(None, None, &bound, ""));
        // A proxy rewrote Host to something unrecognized.
        assert!(host_mismatch(Some("internal-proxy:80"), None, &bound, ""));
        // X-Forwarded-Host alone, also unrecognized.
        assert!(host_mismatch(None, Some("internal-proxy"), &bound, ""));
        // Host is bad but the forwarded one is recognized: no flag (at
        // least one present header matched).
        assert!(!host_mismatch(
            Some("internal-proxy"),
            Some("127.0.0.1"),
            &bound,
            ""
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn host_mismatch_warning_fires_only_once() {
        let flag = std::sync::atomic::AtomicBool::new(false);
        // First detection: the flag was false, so this call would log.
        assert!(!flag.swap(true, std::sync::atomic::Ordering::Relaxed));
        // Every later detection sees the flag already set and skips the log.
        for _ in 0..5 {
            assert!(flag.swap(true, std::sync::atomic::Ordering::Relaxed));
        }
    }
}
