//! The narrow surface the admin panel needs from the rest of the server.
//! Panel handlers that go through [`PanelHost`] instead of `Shared` do not
//! depend on core's world, command or storage internals, so they can move
//! into their own crate with only this trait as the seam.

use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use hyper::StatusCode;
use serde_json::Value;

use tuff_storage::admin::PermissionLevel;

use super::Web;
use super::auth::{PermissionCheck, Principal};
use super::backups::BackupHost;
use super::config::ConfigHost;
use super::dashboard::Point;
use super::datapacks::DatapackHost;
use super::permissions::PermissionHost;
use super::players::PlayerHost;
use super::resource_packs::ResourcePackHost;
use super::sql::SqlHost;

pub type HostFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a console command run from the panel produced.
pub struct CommandOutcome {
    pub feedback: Vec<String>,
    pub succeeded: bool,
}

/// One line of the server log.
pub trait LogRecord: Send + Sync {
    /// Position in the log, from 0.
    fn seq(&self) -> u64;
    fn unix_millis(&self) -> i64;
    fn level(&self) -> log::Level;
    /// The emitting module path, or an explicit log target.
    fn target(&self) -> &str;
    fn text(&self) -> &str;
}

/// How a text field is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextFormat {
    MiniMessage,
    /// `&` color codes.
    Legacy,
    /// Section-sign color codes.
    Section,
    /// MiniMessage tags and legacy codes together.
    Mixed,
}

/// Completions for a partly typed console line.
pub struct Completions {
    pub start: usize,
    pub length: usize,
    pub matches: Vec<String>,
}

/// Why the server refused a panel request.
#[derive(Debug, Clone)]
pub struct HostError {
    pub status: StatusCode,
    pub message: String,
    /// Not worth an audit line: the request named nothing that exists.
    pub quiet: bool,
    /// The change needs the user's confirmation first.
    pub confirm: bool,
}

impl HostError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            quiet: false,
            confirm: false,
        }
    }

    /// A 409 asking the user to confirm the change.
    pub fn needs_confirm(message: impl Into<String>) -> Self {
        Self {
            confirm: true,
            quiet: true,
            ..Self::new(StatusCode::CONFLICT, message)
        }
    }

    pub fn quiet_if(mut self, quiet: bool) -> Self {
        self.quiet = quiet;
        self
    }

    pub fn loud(mut self) -> Self {
        self.quiet = false;
        self
    }
}

/// An edit's result and what the audit log should say about it: no
/// `detail` means the request named nothing and is not audited.
pub struct Edit<T> {
    pub target: String,
    pub detail: Option<String>,
    pub result: Result<T, HostError>,
}

pub trait PanelHost: Send + Sync {
    /// The server itself, for plugin APIs that know its type.
    fn server(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync>;
    /// The HTTP server the panel's routes live on.
    fn http(&self) -> &tuff_http::HttpHandle;
    /// The panel's own actors: sessions, audit log, events, live pages.
    fn web(&self) -> &Web;
    fn config_bool(&self, key: &str) -> Option<bool>;
    fn config_string(&self, key: &str) -> Option<String>;
    fn config_int(&self, key: &str) -> Option<i64>;
    /// Whether the config key holds a secret that logs must redact.
    fn config_secret(&self, key: &str) -> bool;
    /// Addresses the main HTTP listener is bound to.
    fn listener_addrs(&self) -> HostFuture<'_, Vec<SocketAddr>>;
    /// The derived public base URL of the server.
    fn public_base_url(&self) -> String;
    /// The live operator level of a player.
    fn operator_level(&self, uuid: u128) -> HostFuture<'_, PermissionLevel>;
    /// The permission settings now, for one request.
    fn permissions(&self) -> Arc<dyn PermissionCheck>;
    fn timings(&self) -> &tuff_timings::Timings;
    /// The latest metrics snapshot as JSON.
    fn metrics_snapshot(&self) -> Value;
    /// How far back per-second and per-minute history reaches.
    fn history_span(&self) -> (Duration, Duration);
    /// The recorded points of one series over `range`, `None` if unknown.
    fn metrics_history<'a>(
        &'a self,
        series: &'a str,
        range: Duration,
    ) -> HostFuture<'a, Option<Vec<Point>>>;
    fn seed(&self) -> i64;
    /// The World page's summary, without the seed or per-user fields.
    fn world_summary(&self) -> HostFuture<'_, Value>;
    /// The uuid and name of every online player.
    fn online_players(&self) -> HostFuture<'_, Vec<(u128, String)>>;
    /// Runs `line` as `name` at `level`, returning the plain feedback.
    fn run_command(
        self: Arc<Self>,
        line: String,
        name: String,
        level: PermissionLevel,
    ) -> HostFuture<'static, CommandOutcome>;
    fn complete(
        self: Arc<Self>,
        line: String,
        principal: Principal,
        level: PermissionLevel,
    ) -> HostFuture<'static, Completions>;
    /// The log lines still held, oldest first.
    fn log_backfill(&self) -> HostFuture<'_, Vec<Arc<dyn LogRecord>>>;
    /// A text field parsed the way the server will: the component JSON
    /// and its plain text.
    fn text_preview(&self, text: &str, format: TextFormat) -> (Value, String);
    /// Where the world's files live on disk, for free space checks.
    fn world_dir(&self) -> Option<PathBuf>;
    fn backups(&self) -> &dyn BackupHost;
    fn config(&self) -> &dyn ConfigHost;
    fn datapacks(&self) -> &dyn DatapackHost;
    fn permission_settings(&self) -> &dyn PermissionHost;
    fn players(&self) -> &dyn PlayerHost;
    fn resource_packs(&self) -> &dyn ResourcePackHost;
    fn sql(&self) -> &dyn SqlHost;
    /// Holds world saves until the guard drops.
    fn hold_saves(
        self: Arc<Self>,
        reason: &'static str,
        timeout: Duration,
    ) -> HostFuture<'static, Result<Box<dyn Send>, String>>;
}
