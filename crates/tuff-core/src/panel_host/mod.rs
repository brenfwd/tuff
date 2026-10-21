//! The core side of the admin panel's facade: [`PanelHost`] for
//! [`Shared`], over the server's actors and stores.

pub mod backups;
pub mod config;
pub mod datapacks;
pub mod panel;
pub mod permissions;
pub mod players;
pub mod resource_packs;
pub mod sql;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::config::{Key, Value as ConfigValue};
use crate::logging::LogLine;
use crate::metrics::{HISTORY_MINUTES, HISTORY_SECONDS};
use crate::permission::{Change, SetError, Subject, View};
use crate::world::Shared;
use crate::world::messages::CoordinatorMessage;
use tuff_storage::admin::PermissionLevel;
use tuff_web_ui::Web;
use tuff_web_ui::auth::{PermissionCheck, Principal};
use tuff_web_ui::backups::BackupHost;
use tuff_web_ui::config::ConfigHost;
use tuff_web_ui::dashboard::Point;
use tuff_web_ui::datapacks::DatapackHost;
use tuff_web_ui::host::{
    CommandOutcome, Completions, HostFuture, LogRecord, PanelHost, TextFormat,
};
use tuff_web_ui::permissions::PermissionHost;
use tuff_web_ui::players::PlayerHost;
use tuff_web_ui::resource_packs::ResourcePackHost;
use tuff_web_ui::sql::SqlHost;

/// The panel's runtime settings.
pub fn config_keys() -> Vec<Key> {
    vec![
        Key::new(
            tuff_web_ui::ENABLED,
            ConfigValue::Bool(false),
            "Serve the web admin panel under /admin on the HTTP server",
        ),
        Key::new(
            tuff_web_ui::ALLOW_REMOTE,
            ConfigValue::Bool(false),
            "Let non-loopback clients reach the web admin panel (plain HTTP: put it behind a TLS proxy)",
        ),
        Key::new(
            tuff_web_ui::TLS_TERMINATED,
            ConfigValue::Bool(false),
            "Declare that a TLS proxy terminates HTTPS in front of the web admin panel (required with allowRemote unless acknowledgePlaintext is set)",
        ),
        Key::new(
            tuff_web_ui::ACK_PLAINTEXT,
            ConfigValue::Bool(false),
            "Accept serving the web admin panel to remote clients over plain HTTP (required with allowRemote unless tlsTerminated is set)",
        ),
        Key::new(
            tuff_web_ui::HOSTNAMES,
            ConfigValue::String(String::new()),
            "Extra host names the web admin panel answers to, comma separated (IP addresses and localhost always work)",
        ),
        Key::new(
            tuff_web_ui::PUBLIC_URL,
            ConfigValue::String(String::new()),
            "Base URL shown in login messages for the web admin panel (empty derives it)",
        )
        .validated(|v| match v {
            ConfigValue::String(s)
                if s.is_empty() || s.starts_with("http://") || s.starts_with("https://") =>
            {
                Ok(())
            }
            _ => Err("must be empty or an http(s) url".to_owned()),
        }),
        Key::new(
            tuff_web_ui::TRUST_FORWARDED,
            ConfigValue::Bool(false),
            "Take the web admin panel client address and host from X-Forwarded-For/-Host (only behind a proxy that sets them)",
        ),
        Key::new(
            tuff_web_ui::TRUST_OFFLINE_NAMES,
            ConfigValue::Bool(false),
            "Let players who joined with an unauthenticated offline-mode name use the web admin panel (anyone can claim an operator's name)",
        ),
        Key::new(
            tuff_web_ui::MIN_OP_LEVEL,
            ConfigValue::Int(4),
            "Lowest operator level that may log in to the web admin panel",
        )
        .validated(|v| match v {
            ConfigValue::Int(n) if (1..=4).contains(n) => Ok(()),
            _ => Err("must be 1 to 4".to_owned()),
        }),
        backups::config_key(),
    ]
}

/// Registers the panel's guard and routes, with the feature pages and APIs
/// of `panel`, on the main listener. They answer 404 while the panel is
/// disabled, so enabling it is live.
pub async fn start(shared: &Arc<Shared>, panel: &panel::Panel) {
    shared.web.live.set_base(panel.installed()).await;
    let host: Arc<dyn PanelHost> = shared.clone();
    tuff_web_ui::register(&host, Arc::new(panel.installed())).await;
    bridge_logs(shared);
    bridge_metrics(shared);
    tokio::spawn(tuff_web_ui::players::forward(
        Arc::downgrade(&host),
        shared.web.events.clone(),
        tuff_web_ui::players::BRIDGE_PERIOD,
    ));
}

fn bridge_metrics(shared: &Arc<Shared>) {
    let weak = Arc::downgrade(shared);
    tokio::spawn(tuff_web_ui::dashboard::forward_snapshots(
        shared.metrics.subscribe(),
        |snapshot| snapshot.to_json(),
        shared.web.events.clone(),
        move || match weak.upgrade() {
            None => tuff_web_ui::dashboard::Gate::Stop,
            Some(shared) if !tuff_web_ui::enabled(&*shared) => tuff_web_ui::dashboard::Gate::Skip,
            Some(shared) => tuff_web_ui::dashboard::Gate::Publish {
                audit_failures: shared.web.audit.failures(),
            },
        },
    ));
}

fn bridge_logs(shared: &Arc<Shared>) {
    if let Some(hub) = crate::logging::installed() {
        tokio::spawn(forward_logs(hub, shared.web.events.clone()));
    }
}

/// Publishes every line of `hub` on the `logs` topic.
async fn forward_logs(hub: crate::logging::LogHub, events: tuff_web_ui::events::PanelEvents) {
    {
        let Some(sub) = hub.subscribe().await else {
            return;
        };
        let mut live = sub.live;
        loop {
            match live.recv().await {
                Ok(line) => {
                    events.publish("logs", "log", tuff_web_ui::log_json(&*line).to_string());
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    events.publish("logs", "resync", "{}");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    }
}

/// Whose permission nodes a panel principal at `level` uses.
pub fn subject(principal: &Principal, level: PermissionLevel) -> Subject {
    match principal {
        Principal::Console => Subject::Server,
        Principal::Operator { uuid, .. } => Subject::Player { uuid: *uuid, level },
        Principal::Token { .. } => Subject::Anonymous(level),
    }
}

impl PermissionCheck for View {
    fn allows(&self, principal: &Principal, level: PermissionLevel, node: &str) -> bool {
        View::allows(self, subject(principal, level), node)
    }

    fn setting(&self, uuid: u128, node: &str) -> Option<bool> {
        View::setting(self, uuid, node).map(|s| s.held)
    }
}

impl LogRecord for LogLine {
    fn seq(&self) -> u64 {
        self.seq
    }

    fn unix_millis(&self) -> i64 {
        self.unix_millis
    }

    fn level(&self) -> log::Level {
        self.level
    }

    fn target(&self) -> &str {
        &self.target
    }

    fn text(&self) -> &str {
        &self.text
    }
}

/// Applies a permission change and tells affected online players;
/// `Ok(false)` when the settings already were that way.
pub async fn apply_permissions(shared: &Shared, change: Change) -> Result<bool, SetError> {
    let applied = shared.permissions.apply(change).await?;
    if applied.changed {
        crate::permission::notify(&shared.coordinator, &applied.affected).await;
    }
    Ok(applied.changed)
}

pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

impl PanelHost for Shared {
    fn server(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync> {
        self
    }

    fn http(&self) -> &tuff_http::HttpHandle {
        &self.http
    }

    fn web(&self) -> &Web {
        &self.web
    }

    fn config_bool(&self, key: &str) -> Option<bool> {
        self.config.get_bool(key)
    }

    fn config_string(&self, key: &str) -> Option<String> {
        self.config.get_string(key)
    }

    fn config_int(&self, key: &str) -> Option<i64> {
        self.config.get_int(key)
    }

    fn config_secret(&self, key: &str) -> bool {
        self.config.definition(key).is_some_and(|k| k.secret)
    }

    fn listener_addrs(&self) -> HostFuture<'_, Vec<SocketAddr>> {
        Box::pin(self.http.addrs(tuff_http::MAIN_LISTENER))
    }

    fn public_base_url(&self) -> String {
        self.public_base_url.current()
    }

    fn operator_level(&self, uuid: u128) -> HostFuture<'_, PermissionLevel> {
        Box::pin(async move {
            if !self.online_mode
                && !self
                    .config
                    .get_bool(tuff_web_ui::TRUST_OFFLINE_NAMES)
                    .unwrap_or(false)
            {
                return PermissionLevel::All;
            }
            let (reply, rx) = tokio::sync::oneshot::channel();
            let sent = self
                .coordinator
                .send(CoordinatorMessage::PermissionOf { uuid, reply })
                .await;
            if sent.is_err() {
                return PermissionLevel::All;
            }
            rx.await.unwrap_or(PermissionLevel::All)
        })
    }

    fn permissions(&self) -> Arc<dyn PermissionCheck> {
        Arc::new(self.permissions.view())
    }

    fn timings(&self) -> &tuff_timings::Timings {
        &self.timings
    }

    fn metrics_snapshot(&self) -> Value {
        self.metrics.snapshot().to_json()
    }

    fn history_span(&self) -> (Duration, Duration) {
        (
            Duration::from_secs(HISTORY_SECONDS as u64),
            Duration::from_secs(HISTORY_MINUTES as u64 * 60),
        )
    }

    fn metrics_history<'a>(
        &'a self,
        series: &'a str,
        range: Duration,
    ) -> HostFuture<'a, Option<Vec<Point>>> {
        Box::pin(async move {
            let points = self.metrics.history(series, range).await?;
            Some(
                points
                    .into_iter()
                    .map(|p| Point {
                        unix_ms: p.unix_ms,
                        value: p.value,
                        max: p.max,
                    })
                    .collect(),
            )
        })
    }

    fn seed(&self) -> i64 {
        self.seed
    }

    fn world_summary(&self) -> HostFuture<'_, Value> {
        Box::pin(async move {
            let spawn = self.spawn();
            let weather = *self.weather.borrow();
            let clocks = self.clocks.borrow().clone();
            let snapshot = self.metrics.snapshot();
            let dimensions: Vec<Value> = self
                .dimensions
                .iter()
                .map(|d| {
                    let day = d.default_clock.map(|c| clocks.get(c).total_ticks);
                    let loaded = snapshot.dimensions.get(&d.key).cloned().unwrap_or_default();
                    let border = d.border();
                    json!({
                        "key": d.key,
                        "dayTime": day,
                        "loaded": {
                            "regions": loaded.regions,
                            "chunks": loaded.chunks,
                            "entities": loaded.entities,
                            "players": loaded.players,
                        },
                        "border": {
                            "center": border.center,
                            "size": border.size(),
                            "target": border.lerp_target(),
                            "lerpTicks": border.lerp_time(),
                            "damagePerBlock": border.damage_per_block,
                            "safeZone": border.safe_zone,
                            "warningBlocks": border.warning_blocks,
                            "warningTicks": border.warning_time,
                        },
                    })
                })
                .collect();
            let packs = self.datapacks.known();
            let backups = self.backups.list().await.ok();
            json!({
                "backend": self.backend.name(),
                "spawn": {
                    "dimension": spawn.dimension,
                    "position": [spawn.pos.x, spawn.pos.y, spawn.pos.z],
                },
                "gameTime": *self.clock.borrow(),
                "difficulty": self.difficulty.borrow().name(),
                "weather": {
                    "raining": weather.raining,
                    "thundering": weather.thundering,
                    "clearTime": weather.clear_time,
                    "rainTime": weather.rain_time,
                    "thunderTime": weather.thunder_time,
                },
                "dimensions": dimensions,
                "loaded": {
                    "regions": snapshot.world.regions,
                    "chunks": snapshot.world.chunks,
                    "entities": snapshot.world.entities,
                    "players": snapshot.world.players,
                },
                "datapacks": {
                    "available": packs.available.len(),
                    "selected": packs.selected,
                },
                "backups": backups.map(|list| json!({
                    "count": list.len(),
                    "latest": list.first().map(|b| json!({
                        "name": b.name, "size": b.size, "created": b.created_unix,
                    })),
                })),
            })
        })
    }

    fn online_players(&self) -> HostFuture<'_, Vec<(u128, String)>> {
        Box::pin(async move {
            let (reply, online) = tokio::sync::oneshot::channel();
            if self
                .coordinator
                .send(CoordinatorMessage::Players { reply })
                .await
                .is_err()
            {
                return Vec::new();
            }
            online
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|p| (p.uuid, p.name))
                .collect()
        })
    }

    fn run_command(
        self: Arc<Self>,
        line: String,
        name: String,
        level: PermissionLevel,
    ) -> HostFuture<'static, CommandOutcome> {
        Box::pin(async move {
            let (feedback, succeeded) =
                crate::world::commands::console::run_as_counted(self.clone(), &line, name, level)
                    .await;
            CommandOutcome {
                feedback: feedback.iter().map(|c| c.plain()).collect(),
                succeeded: succeeded > 0,
            }
        })
    }

    fn complete(
        self: Arc<Self>,
        line: String,
        principal: Principal,
        level: PermissionLevel,
    ) -> HostFuture<'static, Completions> {
        Box::pin(async move {
            let subject = subject(&principal, level);
            let found = crate::world::commands::console::suggest(&self, &line, subject).await;
            Completions {
                start: found.start,
                length: found.length,
                matches: found.matches,
            }
        })
    }

    fn log_backfill(&self) -> HostFuture<'_, Vec<Arc<dyn LogRecord>>> {
        Box::pin(async move {
            let Some(hub) = crate::logging::installed() else {
                return Vec::new();
            };
            hub.subscribe()
                .await
                .map(|s| s.backfill)
                .unwrap_or_default()
                .into_iter()
                .map(|line| line as Arc<dyn LogRecord>)
                .collect()
        })
    }

    fn text_preview(&self, text: &str, format: TextFormat) -> (Value, String) {
        let component = match format {
            TextFormat::MiniMessage => crate::minimessage::parse(text),
            TextFormat::Legacy => crate::minimessage::from_legacy(text, '&'),
            TextFormat::Section => crate::minimessage::from_legacy(text, '\u{a7}'),
            TextFormat::Mixed => crate::minimessage::parse_mixed(text),
        };
        (component.to_value(), component.plain())
    }

    fn world_dir(&self) -> Option<PathBuf> {
        datapacks::world_dir(self)
    }

    fn backups(&self) -> &dyn BackupHost {
        self
    }

    fn config(&self) -> &dyn ConfigHost {
        self
    }

    fn datapacks(&self) -> &dyn DatapackHost {
        self
    }

    fn permission_settings(&self) -> &dyn PermissionHost {
        self
    }

    fn players(&self) -> &dyn PlayerHost {
        self
    }

    fn resource_packs(&self) -> &dyn ResourcePackHost {
        self
    }

    fn sql(&self) -> &dyn SqlHost {
        self
    }

    fn hold_saves(
        self: Arc<Self>,
        reason: &'static str,
        timeout: Duration,
    ) -> HostFuture<'static, Result<Box<dyn Send>, String>> {
        Box::pin(async move {
            let hold = crate::world::save_gate::hold_saves(&self, reason, timeout)
                .await
                .map_err(|err| err.to_string())?;
            Ok(Box::new(hold) as Box<dyn Send>)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::{HubOptions, LogHub, LogLine};

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn log_lines_reach_the_logs_topic() {
        let hub = LogHub::spawn(HubOptions {
            terminal: false,
            ..HubOptions::default()
        });
        let events = tuff_web_ui::events::PanelEvents::spawn();
        let ctx = tuff_web_ui::AuthContext {
            principal: tuff_web_ui::Principal::Console,
            level: tuff_storage::admin::PermissionLevel::Owners,
            session: "s".into(),
            csrf: String::new(),
            permissions: std::sync::Arc::new(crate::permission::View::default()),
        };
        let mut stream = events.stream(&ctx, "logs", None).await.unwrap();
        tokio::spawn(forward_logs(hub.clone(), events.clone()));
        let mut attempts = 0;
        let event = loop {
            let _ = hub.offer(LogLine::new(log::Level::Warn, "test", "hello panel"));
            if let Ok(Some(event)) =
                tokio::time::timeout(std::time::Duration::from_millis(50), stream.recv()).await
            {
                break event;
            }
            attempts += 1;
            assert!(attempts < 100, "no log event");
        };
        assert_eq!(event.event.as_deref(), Some("log"));
        let line: serde_json::Value = serde_json::from_str(&event.data).unwrap();
        assert_eq!(line["text"], "hello panel");
        assert_eq!(line["level"], "WARN");
        hub.shutdown().await;
    }
}

#[cfg(test)]
mod permission_tests {
    use crate::permission::View;
    use tuff_storage::admin::PermissionLevel;
    use tuff_web_ui::auth::{Principal, Scope, allows, may_use};

    fn op() -> Principal {
        Principal::Operator {
            uuid: 7,
            name: "Alex".into(),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn scopes_follow_levels() {
        let p = op();
        assert!(!allows(
            &View::default(),
            &p,
            PermissionLevel::Moderators,
            Scope::ConsoleRun
        ));
        assert!(allows(
            &View::default(),
            &p,
            PermissionLevel::Gamemasters,
            Scope::ConsoleRun
        ));
        assert!(!allows(
            &View::default(),
            &p,
            PermissionLevel::Admins,
            Scope::PanelAdmin
        ));
        assert!(allows(
            &View::default(),
            &p,
            PermissionLevel::Owners,
            Scope::PanelAdmin
        ));
        assert!(!allows(
            &View::default(),
            &p,
            PermissionLevel::Admins,
            Scope::MetricsRead
        ));
        assert!(allows(
            &View::default(),
            &p,
            PermissionLevel::Owners,
            Scope::MetricsRead
        ));
        assert!(!allows(
            &View::default(),
            &p,
            PermissionLevel::Admins,
            Scope::Plugin("x.y")
        ));
        assert!(allows(
            &View::default(),
            &p,
            PermissionLevel::Owners,
            Scope::Plugin("x.y")
        ));
        assert!(!allows(
            &View::default(),
            &p,
            PermissionLevel::Admins,
            Scope::Timings
        ));
        assert!(allows(
            &View::default(),
            &p,
            PermissionLevel::Owners,
            Scope::Timings
        ));
        assert_eq!(Scope::Plugin("x.y").name(), "x.y");
        assert_eq!(Principal::from_json(&p.to_json()), Some(p));
        assert_eq!(
            Principal::from_json(&Principal::Console.to_json()),
            Some(Principal::Console)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn explicit_settings_decide_scopes_and_panel_access() {
        let p = op();
        let mut grants = crate::permission::Grants::default();
        let mut alex = crate::permission::PlayerGrants::default();
        alex.nodes
            .insert(tuff_web_ui::auth::WEB_ACCESS.into(), true);
        alex.nodes
            .insert(tuff_web_ui::auth::WEB_CONSOLE_RUN.into(), true);
        alex.nodes.insert("tuff.web.metrics.*".into(), false);
        grants.players.insert(7, alex);
        let view = View::default().with_grants(grants);
        let all = PermissionLevel::All;
        assert!(may_use(&view, &p, all, PermissionLevel::Owners));
        assert!(allows(&view, &p, all, Scope::ConsoleRun));
        assert!(!allows(&view, &p, all, Scope::LogsRead));
        assert!(!allows(
            &view,
            &p,
            PermissionLevel::Owners,
            Scope::MetricsRead
        ));
        assert!(allows(
            &view,
            &p,
            PermissionLevel::Owners,
            Scope::PanelAdmin
        ));
        let empty = View::default();
        assert!(!may_use(
            &empty,
            &p,
            PermissionLevel::Admins,
            PermissionLevel::Owners
        ));
        assert!(may_use(
            &empty,
            &p,
            PermissionLevel::Admins,
            PermissionLevel::Admins
        ));
        assert!(may_use(
            &empty,
            &Principal::Console,
            all,
            PermissionLevel::Owners
        ));
        let mut grants = crate::permission::Grants::default();
        let mut denied = crate::permission::PlayerGrants::default();
        denied
            .nodes
            .insert(tuff_web_ui::auth::WEB_ACCESS.into(), false);
        grants.players.insert(7, denied);
        let view = View::default().with_grants(grants);
        assert!(!may_use(
            &view,
            &p,
            PermissionLevel::Owners,
            PermissionLevel::Owners
        ));
    }
}
