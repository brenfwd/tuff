//! The web admin panel: the frontend (a React app built by `build.rs`
//! with bun and Vite into one self-contained HTML file, embedded here)
//! and the server side, served under `/admin` on the main HTTP listener:
//! a JSON API, server-sent events, and the console. Off unless
//! `tuff.web.enabled` is set; even then only loopback clients may use it
//! unless `tuff.web.allowRemote` is set. It reaches the rest of the
//! server only through [`host::PanelHost`].

pub mod access;
pub mod audit;
pub mod auth;
pub mod backups;
pub mod config;
pub mod dashboard;
pub mod datapacks;
pub mod events;
pub mod host;
pub mod live;
pub mod panel;
pub mod permissions;
pub mod players;
pub mod resource_packs;
pub mod sql;
pub mod timings;
pub mod upload;
pub mod world;

mod api;

pub use api::{log_json, register};
mod guard;

/// The whole panel as one HTML document with inline script and style.
pub const INDEX_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/web/index.html"));

/// The sandboxed plugin page runtime as one HTML document.
pub const FRAME_HTML: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/web/frame.html"));

include!(concat!(env!("OUT_DIR"), "/csp.rs"));

use std::net::SocketAddr;
use std::sync::Arc;

use tuff_storage::config::ConfigStore;

pub use auth::{AuthContext, Principal, Scope};

pub const PREFIX: &str = "/admin";
pub const SESSION_ROUTE: &str = "/admin/api/v1/session";
pub const COMMAND_ROUTE: &str = "/admin/api/v1/console/commands";
pub const COMPLETE_ROUTE: &str = "/admin/api/v1/console/complete";
pub const LOGS_ROUTE: &str = "/admin/api/v1/logs";
pub const AUDIT_ROUTE: &str = "/admin/api/v1/audit";
pub const STATUS_ROUTE: &str = "/admin/api/v1/status";
pub const SQL_ROUTE: &str = "/admin/api/v1/sql";
pub const EVENTS_ROUTE: &str = "/admin/events";
pub const PAGES_ROUTE: &str = "/admin/api/v1/pages";
pub const METRICS_ROUTE: &str = "/admin/api/v1/metrics";
pub const METRICS_HISTORY_ROUTE: &str = "/admin/api/v1/metrics/history";
pub const TIMINGS_ROUTE: &str = "/admin/api/v1/timings";
pub const PERMISSIONS_ROUTE: &str = "/admin/api/v1/permissions";
pub const BACKUPS_ROUTE: &str = "/admin/api/v1/backups";
pub const DATAPACKS_ROUTE: &str = "/admin/api/v1/datapacks";
pub const RESOURCE_PACKS_ROUTE: &str = "/admin/api/v1/resourcepacks";
pub const PLAYERS_ROUTE: &str = "/admin/api/v1/players";
pub const CONFIG_ROUTE: &str = "/admin/api/v1/config";
pub const TOKENS_ROUTE: &str = "/admin/api/v1/tokens";
pub const SESSIONS_ROUTE: &str = "/admin/api/v1/sessions";
pub const WORLD_ROUTE: &str = "/admin/api/v1/world";
pub const TEXT_PREVIEW_ROUTE: &str = "/admin/api/v1/text/preview";
/// Every route that needs a session (the session route's POST is the
/// login exchange and GET answers 401 without one).
pub const PRIVATE_ROUTES: [&str; 21] = [
    METRICS_ROUTE,
    TEXT_PREVIEW_ROUTE,
    PLAYERS_ROUTE,
    CONFIG_ROUTE,
    TOKENS_ROUTE,
    SESSIONS_ROUTE,
    WORLD_ROUTE,
    BACKUPS_ROUTE,
    DATAPACKS_ROUTE,
    RESOURCE_PACKS_ROUTE,
    TIMINGS_ROUTE,
    PERMISSIONS_ROUTE,
    METRICS_HISTORY_ROUTE,
    PAGES_ROUTE,
    panel::PAGES_PREFIX,
    COMMAND_ROUTE,
    COMPLETE_ROUTE,
    LOGS_ROUTE,
    AUDIT_ROUTE,
    STATUS_ROUTE,
    EVENTS_ROUTE,
];
pub const ENABLED: &str = "tuff.web.enabled";
pub const ALLOW_REMOTE: &str = "tuff.web.allowRemote";
pub const TLS_TERMINATED: &str = "tuff.web.tlsTerminated";
pub const ACK_PLAINTEXT: &str = "tuff.web.acknowledgePlaintext";
pub const HOSTNAMES: &str = "tuff.web.hostnames";
pub const PUBLIC_URL: &str = "tuff.web.publicUrl";
pub const TRUST_FORWARDED: &str = "tuff.web.trustForwarded";
pub const TRUST_OFFLINE_NAMES: &str = "tuff.web.trustOfflineNames";
pub const MIN_OP_LEVEL: &str = "tuff.web.minOpLevel";
pub const MAX_BACKUP_UPLOAD: &str = backups::MAX_UPLOAD_KEY;

/// The panel's actors: sessions and codes, the audit log, the event bus.
#[derive(Clone)]
pub struct Web {
    pub auth: auth::Auth,
    pub audit: audit::AuditLog,
    pub events: events::PanelEvents,
    /// Pages, APIs and cards that come and go while the server runs.
    pub live: live::LivePanel,
    /// The server file area, where uploaded resource packs live.
    pub files: Option<Arc<dyn ConfigStore>>,
    /// Set once the proxied-host hint has been logged, so it never
    /// repeats per request.
    pub host_mismatch_warned: Arc<std::sync::atomic::AtomicBool>,
}

impl Web {
    /// Starts the actors; `files` keeps sessions and the audit log.
    pub async fn spawn(files: Option<Arc<dyn ConfigStore>>, limits: auth::Limits) -> Web {
        let events = events::PanelEvents::spawn();
        let (published, mut audits) = tokio::sync::mpsc::channel::<serde_json::Value>(256);
        {
            let events = events.clone();
            tokio::spawn(async move {
                while let Some(entry) = audits.recv().await {
                    events.publish("audit", "audit", entry.to_string());
                }
            });
        }
        Web {
            auth: auth::Auth::spawn(files.clone(), limits).await,
            audit: audit::AuditLog::spawn(files.clone(), Some(published)),
            events,
            live: live::LivePanel::spawn(),
            files,
            host_mismatch_warned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Handles nobody answers, for a `Shared` built without a runtime.
    pub fn detached() -> Web {
        Web {
            auth: auth::Auth::detached(),
            audit: audit::AuditLog::detached(),
            events: events::PanelEvents::detached(),
            live: live::LivePanel::detached(),
            files: None,
            host_mismatch_warned: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

tokio::task_local! {
    static PANEL_PRINCIPAL: Principal;
}

/// The panel user a command is running for, if it came from the panel.
pub fn panel_principal() -> Option<Principal> {
    PANEL_PRINCIPAL.try_with(Clone::clone).ok()
}

/// Runs `line` for a panel user at their level, so `/tuff web` and
/// level checks see them rather than the owner console. Returns the plain
/// feedback and whether the command succeeded.
pub async fn run_command(
    host: Arc<dyn host::PanelHost>,
    principal: Principal,
    level: tuff_storage::admin::PermissionLevel,
    line: &str,
) -> (Vec<String>, bool) {
    let name = match &principal {
        Principal::Console => "Panel".to_owned(),
        Principal::Operator { name, .. } => name.clone(),
        Principal::Token { name, .. } => format!("Token:{name}"),
    };
    let outcome = PANEL_PRINCIPAL
        .scope(principal, host.run_command(line.to_owned(), name, level))
        .await;
    (outcome.feedback, outcome.succeeded)
}

/// Whether the panel is switched on.
pub fn enabled(host: &dyn host::PanelHost) -> bool {
    host.config_bool(ENABLED).unwrap_or(false)
}

fn host_url(addr: &SocketAddr) -> String {
    let ip = if addr.ip().is_unspecified() {
        "localhost".to_owned()
    } else if addr.is_ipv6() {
        format!("[{}]", addr.ip())
    } else {
        addr.ip().to_string()
    };
    format!("http://{ip}:{}{PREFIX}/", addr.port())
}

/// The log lines describing the panel's reachability for the given listener
/// addresses.
pub fn status_lines(
    enabled: bool,
    remote: bool,
    addrs: &[SocketAddr],
    public_base: Option<&str>,
) -> Vec<String> {
    if !enabled {
        return vec!["web admin panel disabled".to_owned()];
    }
    let mut lines = vec![format!(
        "web admin panel enabled, remote access {}",
        if remote { "enabled" } else { "disabled" }
    )];
    for addr in addrs {
        let local = addr.ip().is_loopback();
        if local || remote || addr.ip().is_unspecified() {
            let note = if remote && !local { " (remote)" } else { "" };
            lines.push(format!("web admin panel at {}{note}", host_url(addr)));
        }
    }
    if remote && let Some(base) = public_base.filter(|base| !is_local_base(base)) {
        lines.push(format!(
            "web admin panel reachable from outside at {}{PREFIX}/",
            base.trim_end_matches('/')
        ));
    }
    lines
}

fn is_local_base(base: &str) -> bool {
    let host = base.split("://").nth(1).unwrap_or(base);
    host.starts_with("localhost") || host.starts_with("127.") || host.starts_with("[::1]")
}

/// Whether non-loopback clients may reach the panel: `allowRemote` plus
/// either a declared TLS proxy or an explicit plaintext acknowledgement.
pub fn remote_permitted(host: &dyn host::PanelHost) -> bool {
    host.config_bool(ALLOW_REMOTE).unwrap_or(false)
        && (host.config_bool(TLS_TERMINATED).unwrap_or(false)
            || host.config_bool(ACK_PLAINTEXT).unwrap_or(false))
}

pub const REMOTE_REFUSED: &str = "web admin panel: tuff.web.allowRemote is set but the panel only speaks plain HTTP; remote access is refused. Either set tuff.web.tlsTerminated true (a TLS proxy terminates HTTPS in front) or set tuff.web.acknowledgePlaintext true (accept plaintext)";

/// Logs [`status_lines`] for the main listener's current state.
pub async fn announce(host: &dyn host::PanelHost) {
    let addrs = host.listener_addrs().await;
    let remote = remote_permitted(host);
    if enabled(host) && host.config_bool(ALLOW_REMOTE).unwrap_or(false) && !remote {
        log::error!("{REMOTE_REFUSED}");
    }
    let public = host.public_base_url();
    for line in status_lines(enabled(host), remote, &addrs, Some(&public)) {
        log::info!("{line}");
    }
}

/// Where a browser opens the panel, for login messages.
pub async fn public_url(host: &dyn host::PanelHost) -> String {
    let configured = host.config_string(PUBLIC_URL).unwrap_or_default();
    if !configured.is_empty() {
        return format!("{}{PREFIX}/", configured.trim_end_matches('/'));
    }
    let port = host.listener_addrs().await.first().map_or(0, |a| a.port());
    format!("http://localhost:{port}{PREFIX}/")
}

#[cfg(test)]
mod announce_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn status_lines_cover_start_toggle_and_remote() {
        let (http, mut server) = tuff_http::start(None);
        tokio::spawn(async move { server.run().await });
        let cfg = tuff_http::HttpConfig {
            hosts: vec!["127.0.0.1".into()],
            port: 0,
            ..Default::default()
        };
        let addrs = http.bind(tuff_http::MAIN_LISTENER, cfg).await.unwrap();
        let port = addrs[0].port();
        assert_eq!(
            status_lines(false, false, &addrs, None),
            vec!["web admin panel disabled"]
        );
        assert_eq!(
            status_lines(true, false, &addrs, None),
            vec![
                "web admin panel enabled, remote access disabled".to_owned(),
                format!("web admin panel at http://127.0.0.1:{port}/admin/"),
            ]
        );
        let wide: Vec<SocketAddr> = vec![([0, 0, 0, 0], port).into()];
        assert_eq!(
            status_lines(true, true, &wide, Some("http://localhost:1")),
            vec![
                "web admin panel enabled, remote access enabled".to_owned(),
                format!("web admin panel at http://localhost:{port}/admin/ (remote)"),
            ]
        );
        assert_eq!(
            status_lines(true, true, &wide, Some("http://203.0.113.5:8080")),
            vec![
                "web admin panel enabled, remote access enabled".to_owned(),
                format!("web admin panel at http://localhost:{port}/admin/ (remote)"),
                "web admin panel reachable from outside at http://203.0.113.5:8080/admin/"
                    .to_owned(),
            ]
        );
        assert_eq!(
            status_lines(true, false, &wide, Some("http://203.0.113.5:8080")).len(),
            2,
            "no public address while remote access is off"
        );
    }
}
