//! Persists the coordinator's configuration changes through the server's
//! [`ConfigStore`], one request at a time on the blocking pool, so the
//! coordinator never waits on I/O and writes land in the order they were
//! made.
//!
//! The coordinator sends on an unbounded channel: it must never await,
//! requests come only from admin commands (human rate), and each is a
//! snapshot a later one supersedes. Results the coordinator needs (a
//! reloaded whitelist, the end of a [`ConfigRequest::Sync`]) come back as
//! coordinator messages; the actor may await the coordinator's queue, but
//! the coordinator only awaits this actor after it has stopped reading
//! that queue and closed it, so there is no wait cycle.

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::world::messages::CoordinatorMessage;
use crate::world::messages::ListOutcome;
use tuff_storage::admin::{BanEntry, NameAndId, OpEntry, ServerProperties, UserCacheEntry};
use tuff_storage::config::ConfigStore;
use tuff_supervise::{CatchUnwind, count_panic, panic_message};

#[derive(Debug)]
pub enum ConfigRequest {
    Properties(ServerProperties),
    Ops(Vec<OpEntry>),
    Whitelist(Vec<NameAndId>),
    Bans(Vec<BanEntry<NameAndId>>),
    IpBans(Vec<BanEntry<String>>),
    Settings(Vec<(String, String)>),
    UserCache(Vec<UserCacheEntry>),
    Icon(Option<Vec<u8>>),
    /// Answered with [`CoordinatorMessage::WhitelistReloaded`].
    ReloadWhitelist {
        reply: oneshot::Sender<ListOutcome>,
    },
    /// Answered with [`CoordinatorMessage::ConfigSynced`] once every
    /// request before it is done.
    Sync {
        reply: oneshot::Sender<()>,
    },
}

pub type ConfigHandle = mpsc::UnboundedSender<ConfigRequest>;

pub fn start(
    store: Arc<dyn ConfigStore>,
    coordinator: mpsc::Sender<CoordinatorMessage>,
) -> (ConfigHandle, JoinHandle<()>) {
    let (handle, requests) = mpsc::unbounded_channel();
    let task = tokio::spawn(supervise(store, requests, coordinator));
    (handle, task)
}

/// Restarts the loop in place after a panic: it keeps no state between
/// requests, so only the request being handled is lost.
async fn supervise(
    store: Arc<dyn ConfigStore>,
    mut requests: mpsc::UnboundedReceiver<ConfigRequest>,
    coordinator: mpsc::Sender<CoordinatorMessage>,
) {
    loop {
        let served = CatchUnwind::new(Box::pin(run(&store, &mut requests, &coordinator))).await;
        let Err(panic) = served else {
            return;
        };
        count_panic();
        log::error!(
            "the config store actor panicked: {}; restarting it, the request it was handling is lost",
            panic_message(&*panic)
        );
    }
}

async fn run(
    store: &Arc<dyn ConfigStore>,
    requests: &mut mpsc::UnboundedReceiver<ConfigRequest>,
    coordinator: &mpsc::Sender<CoordinatorMessage>,
) {
    while let Some(request) = requests.recv().await {
        let reply = match request {
            ConfigRequest::ReloadWhitelist { reply } => {
                let store = store.clone();
                let whitelist = blocking(move || store.read_whitelist()).await;
                CoordinatorMessage::WhitelistReloaded { whitelist, reply }
            }
            ConfigRequest::Sync { reply } => CoordinatorMessage::ConfigSynced { reply },
            write => {
                let store = store.clone();
                let what = write.name();
                if let Err(err) = blocking(move || apply(&*store, write)).await {
                    log::error!("failed to save {what}: {err}");
                }
                continue;
            }
        };
        let _ = coordinator.send(reply).await;
    }
}

async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> std::io::Result<T> {
    tokio::task::spawn_blocking(job)
        .await
        .map_err(std::io::Error::other)?
}

fn apply(store: &dyn ConfigStore, write: ConfigRequest) -> std::io::Result<()> {
    match write {
        ConfigRequest::Properties(properties) => store.write_properties(&properties),
        ConfigRequest::Ops(ops) => store.write_ops(&ops),
        ConfigRequest::Whitelist(users) => store.write_whitelist(&users),
        ConfigRequest::Bans(bans) => store.write_bans(&bans),
        ConfigRequest::IpBans(bans) => store.write_ip_bans(&bans),
        ConfigRequest::Settings(settings) => store.write_settings(&settings),
        ConfigRequest::UserCache(entries) => store.write_user_cache(&entries),
        ConfigRequest::Icon(png) => store.write_icon(png.as_deref()),
        ConfigRequest::ReloadWhitelist { .. } | ConfigRequest::Sync { .. } => Ok(()),
    }
}

impl ConfigRequest {
    fn name(&self) -> &'static str {
        match self {
            Self::Properties(_) => "server properties",
            Self::Ops(_) => "ops",
            Self::Whitelist(_) => "whitelist",
            Self::Bans(_) => "bans",
            Self::IpBans(_) => "ip bans",
            Self::Settings(_) => "settings",
            Self::UserCache(_) => "user cache",
            Self::Icon(_) => "server icon",
            Self::ReloadWhitelist { .. } | Self::Sync { .. } => "config",
        }
    }
}
