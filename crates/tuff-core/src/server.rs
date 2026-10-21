use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use bon::bon;
use bytes::Bytes;
use thiserror::Error;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;

use crate::auth::{
    GameProfile, MOJANG_PROFILE_API, MOJANG_SESSION_SERVER, SessionService, offline_uuid,
};
use crate::net::admission::{
    Admission, MAX_STATUS_REQUESTS, PRE_LOGIN_FRAME_LIMIT, PreLoginPermit,
};
use crate::net::connection::{Connection, ConnectionReader, Pumped};
use crate::net::crypto::{ServerKeys, minecraft_hex_digest};
use crate::net::tcp_server;
use crate::protocol::configuration::{
    self, ClientInformation, FeatureFlags, FinishConfiguration, KnownPack, KnownPacks,
    LoginSuccess, PluginMessage, RegistryData, UpdateTags,
};
use crate::protocol::handshake::{Intent, Intention};
use crate::protocol::login::{
    EncryptionRequest, EncryptionResponse, LoginAcknowledged, LoginDisconnect, LoginStart,
    SetCompression,
};
use crate::protocol::play;
use crate::protocol::status::{PingRequest, PongResponse, StatusRequest};
use crate::protocol::{PROTOCOL_VERSION, Packet, VERSION_NAME};
use crate::status::{Favicon, ServerListInfo, StatusActor, StatusCommand};
use crate::storage::BackendOptions;
use crate::storage::Storage;
use crate::world::messages::{Joined, PhaseRequest};
use crate::world::{PlayerId, World, WorldConfig};
use tuff_item::text::TextComponent;
use tuff_storage::admin::NameAndId;
use tuff_storage::backend::WorldBackend;
use tuff_storage::config::ConfigStore;
use tuff_trace::ConnState;
use tuff_wire::{PacketReader, PacketWriter};

fn default_binds(network: &crate::world::NetworkSettings) -> Vec<String> {
    let port = network.port;
    let ip = network.ip.trim();
    if ip.is_empty() {
        vec![format!("0.0.0.0:{port}"), format!("[::]:{port}")]
    } else if ip.contains(':') && !ip.starts_with('[') {
        vec![format!("[{ip}]:{port}")]
    } else {
        vec![format!("{ip}:{port}")]
    }
}
/// Deadline for the first packet; a peer that has not spoken by then is dropped.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(100);
const STATUS_COMMAND_QUEUE: usize = 16;
const CONTROL_COMMAND_QUEUE: usize = 8;
/// Most queued frames the writer sends per socket write.
const WRITE_BATCH: usize = 1024;
const SERVER_BRAND: &str = "tuff";
const OFFLINE_MODE_WARNING: &str = "WARNING: online mode is disabled. The server will make no attempt to authenticate usernames; anyone can join under any name and claim an operator's identity. The web admin panel is closed to players unless tuff.web.trustOfflineNames is set. This is not recommended.";

/// Minecraft server instance. Wraps all contexts needed to handle Minecraft clients and worlds, including networking stack, data I/O, terrain generation, chunk and lighting processing, multiplayer considerations, and more. Multiple instances of this struct can exist at once in the same program.
pub struct Server {
    binds: Option<Vec<String>>,
    serve: bool,
    session_server_url: String,
    initial_status: ServerListInfo,
    status_commands: mpsc::Receiver<StatusCommand>,
    control_commands: mpsc::Receiver<ControlCommand>,
    handle: ServerHandle,
    world: WorldConfig,
}

#[bon]
impl Server {
    #[builder]
    pub fn new(
        /// Addresses to listen on. Defaults to all IPv4 and IPv6 interfaces on port 25565.
        binds: Option<Vec<String>>,
        /// Bind no listening sockets, neither the Minecraft port nor the
        /// HTTP server; everything else starts as usual. The server then
        /// runs until shut down, with commands reaching it through
        /// [`ServerHandle::run_command`].
        #[builder(default = true)]
        serve: bool,
        /// The server list message. `motd` from `server.properties` when unset.
        #[builder(into)]
        motd: Option<TextComponent>,
        /// The server list icon. Defaults to `server-icon.png` from the
        /// server's config store (the instance directory, or the server
        /// file for `sqlite`), if there is one.
        favicon: Option<Favicon>,
        /// Player limit. `max-players` from `server.properties` when unset.
        max_players: Option<u32>,
        #[builder(into, default = MOJANG_SESSION_SERVER)] session_server_url: String,
        /// Base URL of the profile API that resolves names of players who
        /// have never joined, for `/op`, `/whitelist add` and the like in
        /// online mode.
        #[builder(into, default = MOJANG_PROFILE_API)]
        profile_api_url: String,
        /// Authenticate players with Mojang's session server. Disabling this
        /// lets anyone join under any name and is not recommended.
        /// `online-mode` from `server.properties` when unset.
        online_mode: Option<bool>,
        /// Directory holding the world directory and the configuration
        /// files, like a vanilla server's working directory. Defaults to the
        /// current directory. Unused for configuration when the storage
        /// backend or `config_store` keeps it.
        instance_dir: Option<PathBuf>,
        /// Directory packet trace files are written to. Defaults to the
        /// process working directory.
        trace_dir: Option<PathBuf>,
        /// `level-name` from `server.properties` when unset.
        #[builder(into)]
        level_name: Option<String>,
        /// Where the world lives, as the storage backend understands it: a
        /// world directory for `vanilla`, the database file for `sqlite`.
        /// Defaults to `<instance_dir>/<level_name>`.
        world_path: Option<PathBuf>,
        /// Chunk view radius. `view-distance` from `server.properties` when unset.
        view_distance: Option<i32>,
        /// Seed for a newly created world; random if unset.
        seed: Option<i64>,
        /// A built-in storage backend by name; `vanilla` (the game's own
        /// world layout) by default.
        #[builder(into)]
        storage_backend: Option<String>,
        /// A storage backend supplied by the embedding program, which takes
        /// precedence over `storage_backend`.
        storage: Option<Arc<dyn WorldBackend>>,
        /// Settings for opening a built-in storage backend.
        #[builder(default)]
        backend_options: BackendOptions,
        /// Keeps `server.properties`, the operator, whitelist and ban lists
        /// and the icon instead of the storage backend (`sqlite` keeps them
        /// in its file) or the instance directory's files.
        config_store: Option<Arc<dyn ConfigStore>>,
        /// Overrides `white-list` from `server.properties` for this run.
        whitelist: Option<bool>,
        /// When ticking waits for every region and player to finish the
        /// previous tick: every tick by default (`always`), or
        /// only during `/tick sprint` (`sprint`).
        #[builder(default)]
        tick_lockstep: crate::world::tick_barrier::TickLockstep,
        /// Config keys and values (`--config <key> <value>`) applied at
        /// start-up for this run only, after the saved settings load.
        #[builder(default)]
        config_overrides: Vec<(String, String)>,
        /// Reads the world but writes nothing to disk (`--no-save`).
        #[builder(default)]
        no_save: bool,
        /// The behavior features to load, in order: there is no dependency
        /// solver, so a feature that must see another's registrations is
        /// listed after it. Empty by default,
        /// which is an inert core: clients connect and chunks stream, but
        /// nothing behaves.
        #[builder(default)]
        features: Vec<Box<dyn crate::feature::Feature>>,
    ) -> Self {
        let instance_dir = instance_dir.unwrap_or_else(|| PathBuf::from("."));
        let mut registries = crate::registry::Registries::new();
        // Infallible constructor by design (`#[builder]` returns `Self`);
        // a feature registration conflict is a startup-fatal
        // misconfiguration, so it panics with the feature and
        // key named rather than limping on.
        if let Err(error) = crate::feature::load_all(&features, &mut registries) {
            panic!("feature load failed: {error}");
        }
        let favicon_missing = favicon.is_none();
        let (status_tx, status_commands) = mpsc::channel(STATUS_COMMAND_QUEUE);
        let (control_tx, control_commands) = mpsc::channel(CONTROL_COMMAND_QUEUE);
        Self {
            binds,
            serve,
            session_server_url: session_server_url.clone(),
            initial_status: ServerListInfo {
                motd: motd
                    .clone()
                    .unwrap_or_else(|| TextComponent::new("A Minecraft Server")),
                favicon,
                max_players: max_players.unwrap_or(20),
                hide_players: false,
                enforces_secure_chat: false,
                players: Vec::new(),
            },
            status_commands,
            control_commands,
            handle: ServerHandle {
                status: status_tx,
                control: control_tx,
                online_mode: Arc::new(watch::Sender::new(online_mode.unwrap_or(true))),
            },
            world: WorldConfig {
                trace_dir: trace_dir.unwrap_or_else(|| PathBuf::from(".")),
                max_players,
                online_mode,
                profile_api_url,
                session_server_url,
                view_distance,
                motd,
                directory: world_path,
                level_name,
                seed,
                storage: match (storage, storage_backend) {
                    (Some(backend), _) => Storage::Custom(backend),
                    (None, Some(name)) => Storage::Named(name),
                    (None, None) => Storage::default(),
                },
                backend_options,
                instance_dir,
                config_store,
                load_icon: favicon_missing,
                whitelist,
                tick_lockstep,
                serve,
                config_overrides,
                no_save,
                registries: Arc::new(registries),
            },
        }
    }

    /// Returns a handle for controlling the server from other tasks. Valid before `run` is called.
    pub fn handle(&self) -> ServerHandle {
        self.handle.clone()
    }

    /// Binds all listeners and serves clients until [`ServerHandle::shutdown`] is called.
    pub async fn run(self) -> Result<()> {
        let Self {
            binds,
            serve,
            session_server_url,
            initial_status,
            status_commands,
            mut control_commands,
            handle,
            world,
        } = self;

        let keys = tokio::task::spawn_blocking(ServerKeys::generate).await??;
        let session = SessionService::new(&session_server_url)?;

        let mut tasks = JoinSet::new();
        let (status_actor, status) = StatusActor::new(initial_status, status_commands);
        let list_info = status_actor.info_receiver();
        let world_list_info = list_info.clone();
        let connection_list = list_info.clone();
        tasks.spawn(tuff_supervise::restarting(
            "the status actor",
            status_actor,
            |actor| Box::pin(actor.run()),
        ));

        let online_mode_override = world.online_mode;
        let profile_api_url = world.profile_api_url.clone();
        let (stop, mut stop_requests) = mpsc::unbounded_channel();
        let world = World::open(world, handle.status.clone(), stop)
            .await
            .context("failed to open the world")?;
        world.attach_server_list(world_list_info);
        if online_mode_override.is_none() {
            handle.online_mode.send_replace(world.network().online_mode);
        }
        let online_mode = handle.online_mode.subscribe();
        let config = world.config();
        let velocity =
            crate::net::velocity::configured(config, world.network().instance_dir.as_deref())?;
        if velocity.is_some() {
            log::info!(
                "Velocity modern forwarding is on: players are accepted only through the proxy"
            );
        } else if !*online_mode.borrow() {
            log::warn!("{OFFLINE_MODE_WARNING}");
            if serve {
                let binds = binds
                    .clone()
                    .unwrap_or_else(|| default_binds(&world.network()));
                let allowed = config.get_bool(crate::config::ALLOW_PUBLIC_OFFLINE);
                crate::net::velocity::refuse_public_offline(allowed == Some(true), &binds)?;
            }
        } else {
            let authority = world.profile_keys();
            tokio::spawn(async move {
                match crate::auth::fetch_profile_authority_keys(&profile_api_url).await {
                    Ok(keys) if !keys.is_empty() => authority.set_keys(keys),
                    Ok(_) => log::warn!(
                        "The profile API listed no player certificate keys; chat sessions are ignored"
                    ),
                    Err(error) => log::warn!(
                        "Could not fetch the keys that sign chat sessions ({error:#}); chat sessions are ignored"
                    ),
                }
            });
        }
        let network = world.network();
        if serve && let Some(query_port) = network.query_port {
            let query = crate::query::QueryServer::bind(
                &crate::query::bind_addrs(&network.ip, query_port),
                crate::query::QueryInfo {
                    list: list_info,
                    map: network.level_name.clone(),
                    host_port: network.port,
                    host_ip: network.ip.clone(),
                },
            )
            .await?;
            for addr in query.local_addrs() {
                log::info!("query listening on {addr}");
            }
            tasks.spawn(query.run());
        }
        let binds = binds.unwrap_or_else(|| default_binds(&network));
        let result = serve_clients(
            serve,
            &binds,
            &mut control_commands,
            &mut stop_requests,
            ConnectionContext {
                keys: Arc::new(keys),
                status,
                list: connection_list,
                session,
                online_mode,
                velocity,
                tracer: world.tracer(),
                metrics: world.metrics(),
                admission: Admission::default(),
                world: world.clone(),
            },
            &mut tasks,
        )
        .await;
        tasks.shutdown().await;
        world.shutdown().await;
        result
    }
}

/// Accepts connections until shutdown is requested.
async fn serve_clients(
    serve: bool,
    binds: &[String],
    control_commands: &mut mpsc::Receiver<ControlCommand>,
    stop_requests: &mut mpsc::UnboundedReceiver<()>,
    context: ConnectionContext,
    tasks: &mut JoinSet<()>,
) -> Result<()> {
    let listeners = if serve {
        tcp_server::bind_all(binds).await?
    } else {
        log::info!("not listening (--no-serve)");
        Vec::new()
    };
    let local_addrs = listeners
        .iter()
        .map(TcpListener::local_addr)
        .collect::<std::io::Result<Vec<_>>>()?;
    for addr in &local_addrs {
        log::info!("listening on {addr}");
    }

    let context = Arc::new(context);
    for listener in listeners {
        tasks.spawn(accept_loop(listener, context.clone()));
    }

    loop {
        tokio::select! {
            command = control_commands.recv() => match command {
                Some(ControlCommand::LocalAddrs(reply)) => {
                    let _ = reply.send(local_addrs.clone());
                }
                Some(ControlCommand::SpawnFake { owner, spec, reply }) => {
                    let world = context.world.clone();
                    tokio::spawn(async move {
                        let made = world
                            .spawn_fake(&owner, spec)
                            .await
                            .unwrap_or_else(|err| Err(err.to_string()));
                        let _ = reply.send(made);
                    });
                }
                Some(ControlCommand::RemoveFakes { owner, uuid, reply }) => {
                    let world = context.world.clone();
                    tokio::spawn(async move {
                        let _ = reply.send(world.remove_fakes(&owner, uuid).await.unwrap_or(0));
                    });
                }
                Some(ControlCommand::HttpAddrs(reply)) => {
                    let world = context.world.clone();
                    tokio::spawn(async move {
                        let _ = reply.send(world.http_addrs().await);
                    });
                }
                Some(ControlCommand::ResourcePacks(reply)) => {
                    let _ = reply.send(context.world.resource_packs());
                }
                Some(ControlCommand::CustomPack(reply)) => {
                    let _ = reply.send(context.world.custom_pack());
                }
                Some(ControlCommand::Metrics(reply)) => {
                    let _ = reply.send(context.metrics.clone());
                }
                Some(ControlCommand::Timings(reply)) => {
                    let _ = reply.send(context.world.timings());
                }
                Some(ControlCommand::HoldSaves { owner, limit, reply }) => {
                    let world = context.world.clone();
                    tokio::spawn(async move {
                        let _ = reply.send(world.hold_saves(&owner, limit).await);
                    });
                }
                Some(ControlCommand::TickTimes(reply)) => {
                    let _ = reply.send(context.world.tick_times());
                }
                Some(ControlCommand::PublicBaseUrl(reply)) => {
                    let _ = reply.send(context.world.public_base_url());
                }
                Some(ControlCommand::CompleteCommand { line, reply }) => {
                    let world = context.world.clone();
                    tokio::spawn(async move {
                        let _ = reply.send(world.complete_console_command(&line).await);
                    });
                }
                Some(ControlCommand::RunCommand { line, source, operator, reply }) => {
                    let world = context.world.clone();
                    tokio::spawn(async move {
                        let run = async move {
                            match source {
                                None => world.run_console_command(&line).await,
                                Some((name, level)) => {
                                    world.run_command_as(&line, name, level).await
                                }
                            }
                        };
                        let feedback = if operator {
                            OPERATOR_COMMAND.scope((), run).await
                        } else {
                            run.await
                        };
                        let _ = reply.send(feedback);
                    });
                }
                Some(ControlCommand::WebLoginCode(principal, reply)) => {
                    let web = context.world.web();
                    tokio::spawn(async move {
                        let _ = reply.send(web.auth.new_code(principal).await);
                    });
                }
                Some(ControlCommand::WorldShared(reply)) => {
                    let _ = reply.0.send(context.world.shared().clone());
                }
                Some(ControlCommand::Shutdown) | None => break,
            },
            _ = stop_requests.recv() => break,
        }
    }
    Ok(())
}

tokio::task_local! {
    static OPERATOR_COMMAND: ();
}

/// True inside a command run through `ServerHandle::run_operator_command`.
pub fn is_operator_command() -> bool {
    OPERATOR_COMMAND.try_with(|_| ()).is_ok()
}

#[derive(Debug)]
enum ControlCommand {
    LocalAddrs(oneshot::Sender<Vec<SocketAddr>>),
    SpawnFake {
        owner: String,
        spec: crate::world::fake::FakeSpec,
        reply: oneshot::Sender<Result<crate::world::fake::FakeHandle, String>>,
    },
    RemoveFakes {
        owner: String,
        uuid: Option<u128>,
        reply: oneshot::Sender<usize>,
    },
    /// Addresses the HTTP server's main listener is bound to.
    HttpAddrs(oneshot::Sender<Vec<SocketAddr>>),
    PublicBaseUrl(oneshot::Sender<crate::public_ip::BaseUrl>),
    TickTimes(oneshot::Sender<tokio::sync::broadcast::Receiver<crate::world::tick_rate::TickTime>>),
    ResourcePacks(oneshot::Sender<crate::resource_pack::ResourcePacks>),
    CustomPack(oneshot::Sender<crate::custom_pack::CustomPack>),
    Metrics(oneshot::Sender<crate::metrics::Metrics>),
    Timings(oneshot::Sender<tuff_timings::Timings>),
    HoldSaves {
        owner: String,
        limit: std::time::Duration,
        reply: oneshot::Sender<HoldResult>,
    },
    CompleteCommand {
        line: String,
        reply: oneshot::Sender<crate::command::Suggestions>,
    },
    /// A console command line; the reply carries its feedback.
    RunCommand {
        line: String,
        /// Run as this name and level instead of the owner console.
        source: Option<(String, tuff_storage::admin::PermissionLevel)>,
        operator: bool,
        reply: oneshot::Sender<Vec<TextComponent>>,
    },
    WebLoginCode(tuff_web_ui::Principal, oneshot::Sender<Option<String>>),
    WorldShared(SharedReply),
    Shutdown,
}

/// Where [`ControlCommand::WorldShared`] answers.
pub(crate) struct SharedReply(oneshot::Sender<Arc<crate::world::Shared>>);

impl std::fmt::Debug for SharedReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharedReply")
    }
}

type HoldResult = Result<crate::world::save_gate::SaveHold, crate::world::save_gate::HoldError>;

#[derive(Debug, Error)]
#[error("server is not running")]
pub struct ServerStopped;

/// Cloneable handle for sending commands into a running [`Server`].
#[derive(Clone)]
pub struct ServerHandle {
    status: mpsc::Sender<StatusCommand>,
    control: mpsc::Sender<ControlCommand>,
    online_mode: Arc<watch::Sender<bool>>,
}

impl ServerHandle {
    pub async fn set_motd(&self, motd: impl Into<TextComponent>) -> Result<(), ServerStopped> {
        self.send_status(StatusCommand::SetMotd(motd.into())).await
    }

    pub async fn set_favicon(&self, favicon: Option<Favicon>) -> Result<(), ServerStopped> {
        self.send_status(StatusCommand::SetFavicon(favicon)).await
    }

    pub async fn set_max_players(&self, max_players: u32) -> Result<(), ServerStopped> {
        self.send_status(StatusCommand::SetMaxPlayers(max_players))
            .await
    }

    /// Enables or disables authentication for subsequent logins. Players
    /// already connected are unaffected. Disabling it is not recommended.
    pub fn set_online_mode(&self, online_mode: bool) {
        let previous = self.online_mode.send_replace(online_mode);
        if previous && !online_mode {
            log::warn!("{OFFLINE_MODE_WARNING}");
        }
    }

    pub fn online_mode(&self) -> bool {
        *self.online_mode.borrow()
    }

    /// Runs a command line as the server console (with or without the
    /// leading `/`), returning the feedback lines it produced.
    pub async fn run_command(
        &self,
        line: impl Into<String>,
    ) -> Result<Vec<TextComponent>, ServerStopped> {
        let (reply, response) = oneshot::channel();
        self.send_control(ControlCommand::RunCommand {
            line: line.into(),
            source: None,
            operator: false,
            reply,
        })
        .await?;
        response.await.map_err(|_| ServerStopped)
    }

    /// Completions for a partly typed console line. `start` counts from
    /// the start of `line`, with or without a leading `/`.
    pub async fn complete_command(
        &self,
        line: impl Into<String>,
    ) -> Result<crate::command::Suggestions, ServerStopped> {
        let (reply, response) = oneshot::channel();
        self.send_control(ControlCommand::CompleteCommand {
            line: line.into(),
            reply,
        })
        .await?;
        response.await.map_err(|_| ServerStopped)
    }

    /// Runs a console command line supplied by the operator on the command
    /// line; commands can see this through `is_operator_command`.
    pub async fn run_operator_command(
        &self,
        line: impl Into<String>,
    ) -> Result<Vec<TextComponent>, ServerStopped> {
        let (reply, response) = oneshot::channel();
        self.send_control(ControlCommand::RunCommand {
            line: line.into(),
            source: None,
            operator: true,
            reply,
        })
        .await?;
        response.await.map_err(|_| ServerStopped)
    }

    /// Runs a command line as a console-like source named `name` with
    /// permission `level`, so level checks and operator feedback apply as
    /// for that source; returns the feedback lines.
    pub async fn run_command_as(
        &self,
        line: impl Into<String>,
        name: impl Into<String>,
        level: tuff_storage::admin::PermissionLevel,
    ) -> Result<Vec<TextComponent>, ServerStopped> {
        let (reply, response) = oneshot::channel();
        self.send_control(ControlCommand::RunCommand {
            line: line.into(),
            source: Some((name.into(), level)),
            operator: false,
            reply,
        })
        .await?;
        response.await.map_err(|_| ServerStopped)
    }

    /// Admits a connectionless fake player owned by `owner`.
    pub async fn spawn_fake(
        &self,
        owner: impl Into<String>,
        spec: crate::world::fake::FakeSpec,
    ) -> Result<Result<crate::world::fake::FakeHandle, String>, ServerStopped> {
        let (reply, response) = oneshot::channel();
        self.send_control(ControlCommand::SpawnFake {
            owner: owner.into(),
            spec,
            reply,
        })
        .await?;
        response.await.map_err(|_| ServerStopped)
    }

    /// Removes a fake of `owner` by uuid, or all of them.
    pub async fn remove_fakes(
        &self,
        owner: impl Into<String>,
        uuid: Option<u128>,
    ) -> Result<usize, ServerStopped> {
        let (reply, response) = oneshot::channel();
        self.send_control(ControlCommand::RemoveFakes {
            owner: owner.into(),
            uuid,
            reply,
        })
        .await?;
        response.await.map_err(|_| ServerStopped)
    }

    /// The addresses the server is actually listening on (useful when binding port 0).
    pub async fn local_addrs(&self) -> Result<Vec<SocketAddr>, ServerStopped> {
        let (reply, response) = oneshot::channel();
        self.send_control(ControlCommand::LocalAddrs(reply)).await?;
        response.await.map_err(|_| ServerStopped)
    }

    /// Addresses the HTTP server's main listener is bound to (empty if it
    /// failed to bind); useful when `tuff.http.port` is 0.
    pub async fn http_addrs(&self) -> Result<Vec<SocketAddr>, ServerStopped> {
        let (reply, response) = oneshot::channel();
        self.send_control(ControlCommand::HttpAddrs(reply)).await?;
        response.await.map_err(|_| ServerStopped)
    }

    /// Watchable base URL clients fetch served resources from.
    pub async fn public_base_url(&self) -> Result<crate::public_ip::BaseUrl, ServerStopped> {
        let (reply, response) = oneshot::channel();
        self.send_control(ControlCommand::PublicBaseUrl(reply))
            .await?;
        response.await.map_err(|_| ServerStopped)
    }

    /// Every finished tick's duration from now on, for benchmarks and
    /// monitoring; see [`crate::world::World::tick_times`].
    pub async fn tick_times(
        &self,
    ) -> Result<tokio::sync::broadcast::Receiver<crate::world::tick_rate::TickTime>, ServerStopped>
    {
        let (reply, response) = oneshot::channel();
        self.send_control(ControlCommand::TickTimes(reply)).await?;
        response.await.map_err(|_| ServerStopped)
    }

    /// Live metrics: snapshots, history and feature series.
    pub async fn metrics(&self) -> Result<crate::metrics::Metrics, ServerStopped> {
        let (reply, response) = oneshot::channel();
        self.send_control(ControlCommand::Metrics(reply)).await?;
        response.await.map_err(|_| ServerStopped)
    }

    /// The timings controller: record, stop and fetch reports.
    pub async fn timings(&self) -> Result<tuff_timings::Timings, ServerStopped> {
        let (reply, response) = oneshot::channel();
        self.send_control(ControlCommand::Timings(reply)).await?;
        response.await.map_err(|_| ServerStopped)
    }

    /// Saves and flushes the world, then keeps every write away from its
    /// files until the returned hold is released or dropped (or `limit`
    /// passes), so they can be copied consistently.
    pub async fn hold_saves(
        &self,
        owner: impl Into<String>,
        limit: std::time::Duration,
    ) -> Result<HoldResult, ServerStopped> {
        let (reply, response) = oneshot::channel();
        self.send_control(ControlCommand::HoldSaves {
            owner: owner.into(),
            limit,
            reply,
        })
        .await?;
        response.await.map_err(|_| ServerStopped)
    }

    /// The hosted resource packs: register, update, push and pop at runtime.
    pub async fn resource_packs(
        &self,
    ) -> Result<crate::resource_pack::ResourcePacks, ServerStopped> {
        let (reply, response) = oneshot::channel();
        self.send_control(ControlCommand::ResourcePacks(reply))
            .await?;
        response.await.map_err(|_| ServerStopped)
    }

    /// The generated custom content pack: replace asset sets at runtime.
    pub async fn custom_pack(&self) -> Result<crate::custom_pack::CustomPack, ServerStopped> {
        let (reply, response) = oneshot::channel();
        self.send_control(ControlCommand::CustomPack(reply)).await?;
        response.await.map_err(|_| ServerStopped)
    }

    /// A one-time web panel login code for `principal`, as `/tuff web
    /// login` makes; `None` if the panel's auth actor is gone.
    pub async fn web_login_code(
        &self,
        principal: tuff_web_ui::Principal,
    ) -> Result<Option<String>, ServerStopped> {
        let (reply, response) = oneshot::channel();
        self.send_control(ControlCommand::WebLoginCode(principal, reply))
            .await?;
        response.await.map_err(|_| ServerStopped)
    }

    /// Stops accepting connections and makes [`Server::run`] return.
    pub async fn shutdown(&self) -> Result<(), ServerStopped> {
        self.send_control(ControlCommand::Shutdown).await
    }

    async fn send_status(&self, command: StatusCommand) -> Result<(), ServerStopped> {
        self.status.send(command).await.map_err(|_| ServerStopped)
    }

    /// The running world's shared state.
    pub async fn world_shared(&self) -> Result<Arc<crate::world::Shared>, ServerStopped> {
        let (reply, response) = oneshot::channel();
        self.send_control(ControlCommand::WorldShared(SharedReply(reply)))
            .await?;
        response.await.map_err(|_| ServerStopped)
    }

    async fn send_control(&self, command: ControlCommand) -> Result<(), ServerStopped> {
        self.control.send(command).await.map_err(|_| ServerStopped)
    }
}

/// Immutable state shared by every connection task.
struct ConnectionContext {
    keys: Arc<ServerKeys>,
    status: watch::Receiver<Bytes>,
    list: watch::Receiver<Arc<ServerListInfo>>,
    session: SessionService,
    online_mode: watch::Receiver<bool>,
    velocity: Option<crate::net::velocity::Secret>,
    world: World,
    tracer: tuff_trace::Tracer,
    metrics: crate::metrics::Metrics,
    admission: Admission,
}

fn disconnect_message(addr: Option<SocketAddr>, err: &anyhow::Error) -> String {
    match addr {
        Some(addr) => format!("client {addr} disconnected: {err:#}"),
        None => format!("client disconnected: {err:#}"),
    }
}

async fn accept_loop(listener: TcpListener, context: Arc<ConnectionContext>) {
    loop {
        match listener.accept().await {
            Ok((socket, addr)) => {
                let context = context.clone();
                let network = context.world.network();
                let Some(permit) = context.admission.admit(
                    addr.ip(),
                    network.pre_login_max_connections,
                    network.pre_login_max_per_ip,
                ) else {
                    log::debug!("pre-login connection cap reached, dropping a connection");
                    continue;
                };
                tokio::spawn(async move {
                    if let Err(err) = handle_connection(&context, socket, permit).await {
                        let shown = context.world.network().log_ips.then_some(addr);
                        log::info!("{}", disconnect_message(shown, &err));
                    }
                });
            }
            Err(err) => {
                log::error!("accept failed: {err}");
                tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
            }
        }
    }
}

async fn handle_connection(
    context: &ConnectionContext,
    socket: TcpStream,
    permit: PreLoginPermit,
) -> Result<()> {
    socket.set_nodelay(true)?;
    let address = socket.peer_addr()?.ip();
    let list = context.list.borrow().clone();
    let Some(socket) = crate::net::legacy_ping::try_answer(socket, &list).await? else {
        return Ok(());
    };
    let timeout = context.world.network().pre_login_timeout;
    let joined = tokio::time::timeout(timeout, async {
        let mut conn = Connection::new(socket);
        conn.set_frame_limit(Some(PRE_LOGIN_FRAME_LIMIT));
        conn.set_tracer(context.tracer.clone());
        conn.set_metrics(&context.metrics);
        let intention: Intention = tokio::time::timeout(HANDSHAKE_TIMEOUT, conn.expect())
            .await
            .map_err(|_| anyhow::anyhow!("handshake timed out"))??;
        conn.trace_state(match intention.intent {
            Intent::Status => ConnState::Status,
            _ => ConnState::Login,
        });
        match intention.intent {
            Intent::Status if !context.world.network().enable_status => Ok(None),
            Intent::Status => handle_status(context, conn).await.map(|_| None),
            Intent::Login => handle_login(context, conn, intention.protocol_version, address).await,
            Intent::Transfer if !context.world.network().accepts_transfers => disconnect(
                conn,
                TextComponent::translate("multiplayer.disconnect.transfers_disabled", []),
            )
            .await
            .map(|_| None),
            Intent::Transfer => {
                handle_login(context, conn, intention.protocol_version, address).await
            }
        }
    })
    .await
    .unwrap_or_else(|_| Err(anyhow::anyhow!("timed out")))?;
    drop(permit);
    match joined {
        Some((mut conn, profile, client, address)) => {
            conn.set_frame_limit(None);
            play(context, conn, profile, client, address).await
        }
        None => Ok(()),
    }
}

async fn handle_status(context: &ConnectionContext, mut conn: Connection) -> Result<()> {
    let mut requests = 0u32;
    while let Some(payload) = conn.read_packet().await? {
        let mut reader = PacketReader::new(payload);
        match reader.read_var_int()? {
            StatusRequest::ID => {
                requests += 1;
                anyhow::ensure!(requests <= MAX_STATUS_REQUESTS, "too many status requests");
                let snapshot = context.status.borrow().clone();
                conn.send_raw(&snapshot).await?;
            }
            PingRequest::ID => {
                let ping = PingRequest::decode(&mut reader)?;
                conn.send(&PongResponse {
                    timestamp: ping.timestamp,
                })
                .await?;
                return conn.close().await;
            }
            id => anyhow::bail!("unexpected status packet {id:#04x}"),
        }
    }
    Ok(())
}

fn valid_offline_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 16 && name.chars().all(|c| c.is_ascii_graphic())
}

/// Authenticates the player (in online mode) and runs the configuration
/// phase. Returns `None` if the player was turned away.
async fn handle_login(
    context: &ConnectionContext,
    mut conn: Connection,
    protocol_version: i32,
    address: std::net::IpAddr,
) -> Result<Option<(Connection, GameProfile, ClientDetails, std::net::IpAddr)>> {
    if protocol_version != PROTOCOL_VERSION {
        let key = if protocol_version < PROTOCOL_VERSION {
            "multiplayer.disconnect.outdated_client"
        } else {
            "multiplayer.disconnect.incompatible"
        };
        disconnect(
            conn,
            TextComponent::translate(key, [TextComponent::new(VERSION_NAME)]),
        )
        .await?;
        return Ok(None);
    }
    let login_start: LoginStart = conn.expect().await?;
    conn.trace_player(&login_start.name);

    let online_mode = *context.online_mode.borrow();
    let mut address = address;
    let profile = if let Some(secret) = &context.velocity {
        match crate::net::velocity::forwarded_identity(secret, &mut conn, &login_start.name).await?
        {
            Ok(forwarded) => {
                address = forwarded.address;
                forwarded.profile
            }
            Err(reason) => {
                disconnect(conn, reason).await?;
                return Ok(None);
            }
        }
    } else if online_mode {
        match authenticate(context, &mut conn, &login_start, address).await? {
            Ok(profile) => profile,
            Err(reason) => {
                disconnect(conn, reason).await?;
                return Ok(None);
            }
        }
    } else {
        if !valid_offline_name(&login_start.name) {
            disconnect(
                conn,
                TextComponent::translate("multiplayer.disconnect.invalid_player_data", []),
            )
            .await?;
            return Ok(None);
        }
        GameProfile {
            id: offline_uuid(&login_start.name),
            name: login_start.name.clone(),
            properties: Vec::new(),
        }
    };
    let user = NameAndId {
        uuid: profile.id,
        name: profile.name.clone(),
    };
    if let Err(reason) = context.world.can_login(user, address).await? {
        disconnect(conn, reason).await?;
        return Ok(None);
    }
    log::info!("{} logged in with UUID {:032x}", profile.name, profile.id);

    let threshold = context.world.network().compression_threshold;
    if threshold >= 0 {
        conn.send(&SetCompression { threshold }).await?;
        conn.enable_compression(threshold as usize);
    }
    conn.send(&LoginSuccess {
        profile: &profile,
        session_id: rand::random(),
    })
    .await?;
    conn.expect::<LoginAcknowledged>().await?;
    conn.trace_state(ConnState::Configuration);

    let client = configure(context, &mut conn, profile.id).await?;
    Ok(Some((conn, profile, client, address)))
}

async fn authenticate(
    context: &ConnectionContext,
    conn: &mut Connection,
    login_start: &LoginStart,
    address: std::net::IpAddr,
) -> Result<Result<GameProfile, TextComponent>> {
    let verify_token: [u8; 4] = rand::random();
    conn.send(&EncryptionRequest {
        server_id: String::new(),
        public_key: context.keys.public_der().clone(),
        verify_token: Bytes::copy_from_slice(&verify_token),
        should_authenticate: true,
    })
    .await?;

    let response: EncryptionResponse = conn.expect().await?;
    let decrypted_token = context.keys.decrypt(&response.verify_token)?;
    ensure!(decrypted_token == verify_token, "verify token mismatch");
    let shared_secret: [u8; 16] = context
        .keys
        .decrypt(&response.shared_secret)?
        .try_into()
        .ok()
        .context("shared secret must be 16 bytes")?;
    conn.enable_encryption(&shared_secret);

    let server_hash = minecraft_hex_digest("", &shared_secret, context.keys.public_der());
    Ok(
        match context
            .session
            .has_joined(
                &login_start.name,
                &server_hash,
                context
                    .world
                    .network()
                    .prevent_proxy_connections
                    .then_some(address),
            )
            .await
        {
            Ok(Some(profile)) => Ok(profile),
            Ok(None) => Err(TextComponent::new("Failed to verify username!")),
            Err(err) => {
                log::warn!("session server error for {}: {err:#}", login_start.name);
                Err(TextComponent::new(
                    "Authentication servers are down. Please try again later, sorry!",
                ))
            }
        },
    )
}

/// Negotiates the vanilla `minecraft:core` pack so registry contents come
/// from the client's own copy, then lists registry entries and tags.
async fn configure(
    context: &ConnectionContext,
    conn: &mut Connection,
    player: u128,
) -> Result<ClientDetails> {
    let mut client = ClientDetails::default();
    synchronize(context, conn, &mut client).await?;
    negotiate_code_of_conduct(context, conn, &mut client).await?;
    negotiate_resource_packs(context, conn, player, &mut client).await?;
    finish_configuration(conn, &mut client).await?;
    Ok(client)
}

/// Sends the brand, feature flags, registries, tags and server links,
/// as at the start of every configuration phase.
async fn synchronize(
    context: &ConnectionContext,
    conn: &mut Connection,
    client: &mut ClientDetails,
) -> Result<()> {
    let mut brand = PacketWriter::new();
    brand.write_string(SERVER_BRAND);
    conn.send(&PluginMessage {
        channel: "minecraft:brand",
        data: &brand.into_bytes(),
    })
    .await?;
    let features = context.world.features();
    let flags: Vec<&str> = features.iter().map(String::as_str).collect();
    conn.send(&FeatureFlags(&flags)).await?;
    let core = KnownPack {
        namespace: "minecraft".into(),
        id: "core".into(),
        version: VERSION_NAME.into(),
    };
    conn.send(&KnownPacks(std::slice::from_ref(&core))).await?;

    loop {
        match next_configuration_packet(conn).await? {
            configuration::Serverbound::KnownPacks(packs) => {
                ensure!(
                    packs.contains(&core),
                    "client does not have the {VERSION_NAME} core data pack"
                );
                break;
            }
            other => client.absorb(other),
        }
    }

    let custom = context.world.custom_dimensions();
    let registries = context.world.registries();
    for registry in tuff_vanilla_data::data_registry::SYNCHRONIZED {
        let extra = custom.registry_entries(registry.key);
        match registries.get(registry.key) {
            Some(frozen) => {
                conn.send(&configuration::RuntimeRegistryData {
                    registry: frozen,
                    extra: &extra,
                })
                .await?
            }
            None => {
                conn.send(&RegistryData {
                    registry: registry.key,
                    entries: registry.entries,
                    custom: &extra,
                })
                .await?
            }
        }
    }
    conn.send(&UpdateTags(context.world.tags_packet())).await?;
    send_server_links(context, conn).await
}

async fn finish_configuration(conn: &mut Connection, client: &mut ClientDetails) -> Result<()> {
    conn.send(&FinishConfiguration).await?;
    loop {
        match next_configuration_packet(conn).await? {
            configuration::Serverbound::AcknowledgeFinish => {
                conn.trace_state(ConnState::Play);
                return Ok(());
            }
            other => client.absorb(other),
        }
    }
}

/// What the client told us about itself during configuration.
#[derive(Default)]
struct ClientDetails {
    info: ClientInformation,
    language: String,
    brand: Option<String>,
}

impl ClientDetails {
    fn absorb(&mut self, packet: configuration::Serverbound) {
        match packet {
            configuration::Serverbound::ClientInformation(update, language) => {
                self.info = update;
                self.language = language;
            }
            configuration::Serverbound::Brand(brand) => self.brand = Some(brand),
            _ => {}
        }
    }
}

async fn send_server_links(context: &ConnectionContext, conn: &mut Connection) -> Result<()> {
    let links = &context.world.network().server_links;
    if !links.is_empty() {
        conn.send(&configuration::ServerLinks(links)).await?;
    }
    Ok(())
}

/// Shows the code of conduct and waits for the client to accept it.
async fn negotiate_code_of_conduct(
    context: &ConnectionContext,
    conn: &mut Connection,
    client: &mut ClientDetails,
) -> Result<()> {
    let network = context.world.network();
    let Some(text) = crate::code_of_conduct::pick(&network.code_of_conduct, &client.language)
    else {
        return Ok(());
    };
    conn.send(&configuration::CodeOfConduct(text)).await?;
    loop {
        match next_configuration_packet(conn).await? {
            configuration::Serverbound::AcceptCodeOfConduct => return Ok(()),
            other => client.absorb(other),
        }
    }
}

const RESOURCE_PACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Offers the join packs and waits until each one reaches a final status.
/// A required pack the client declines, fails to load or never answers for
/// ends the connection; optional ones are given up on.
async fn negotiate_resource_packs(
    context: &ConnectionContext,
    conn: &mut Connection,
    player: u128,
    client: &mut ClientDetails,
) -> Result<()> {
    use crate::protocol::pack::{ConfigDisconnect, ConfigPush, Push};
    let packs = context.world.resource_packs();
    let mut pending = std::collections::HashMap::new();
    let configured = context.world.network().resource_pack.clone();
    for offer in configured.into_iter().chain(packs.join_offers().await) {
        conn.send(&ConfigPush(Push {
            id: offer.id,
            url: &offer.url,
            hash: &offer.sha1,
            required: offer.required,
            prompt: offer.prompt.as_ref(),
        }))
        .await?;
        pending.insert(offer.id, (offer.required, offer.sha1));
    }
    let deadline = tokio::time::Instant::now() + RESOURCE_PACK_TIMEOUT;
    let refuse = TextComponent::translate("multiplayer.requiredTexturePrompt.disconnect", []);
    while !pending.is_empty() {
        let packet = match tokio::time::timeout_at(deadline, next_configuration_packet(conn)).await
        {
            Ok(packet) => packet?,
            Err(_) => {
                if pending.values().any(|(required, _)| *required) {
                    conn.send(&ConfigDisconnect(&refuse)).await?;
                    bail!("required resource pack was not answered in time");
                }
                return Ok(());
            }
        };
        match packet {
            configuration::Serverbound::ResourcePack {
                id,
                status: Some(status),
            } => {
                let Some((required, hash)) = pending.get(&id) else {
                    continue;
                };
                let required = *required;
                packs
                    .report_join(
                        crate::resource_pack::PackEvent {
                            player,
                            pack: id,
                            status,
                        },
                        hash,
                    )
                    .await;
                if required && status.is_failure() {
                    conn.send(&ConfigDisconnect(&refuse)).await?;
                    bail!("client refused required resource pack");
                }
                if status.is_final() {
                    pending.remove(&id);
                }
            }
            other => client.absorb(other),
        }
    }
    Ok(())
}

async fn next_configuration_packet(conn: &mut Connection) -> Result<configuration::Serverbound> {
    let payload = conn
        .read_packet()
        .await?
        .context("connection closed during configuration")?;
    let packet = configuration::Serverbound::decode(payload)?;
    Ok(packet)
}

/// Hands the player to the world and pumps packets both ways until either
/// side ends the connection. A [`PhaseRequest::Configure`] takes the player
/// out of the world into configuration and back in again.
async fn play(
    context: &ConnectionContext,
    mut conn: Connection,
    profile: GameProfile,
    mut client: ClientDetails,
    address: std::net::IpAddr,
) -> Result<()> {
    // Unbounded so the world never waits on a client; the writer task caps
    // what a client that stops reading can leave queued.
    let limit = context
        .world
        .config()
        .get_int(crate::config::MAX_OUTBOUND_BYTES)
        .unwrap_or(crate::config::DEFAULT_MAX_OUTBOUND_BYTES)
        .max(1) as usize;
    // Unbounded: phase requests are rare and come from commands that must
    // not wait on a busy connection.
    let (phase, mut phases) = mpsc::unbounded_channel();
    loop {
        // A fresh channel per stay in the world, so nothing queued for an
        // earlier stay can reach the client after it leaves play.
        let (outbound, mut outbound_rx) = mpsc::unbounded_channel::<Bytes>();
        let joined = match context
            .world
            .join(
                profile.clone(),
                client.info,
                client.brand.clone(),
                address,
                outbound,
                phase.clone(),
            )
            .await?
        {
            Ok(joined) => joined,
            Err(reason) => {
                conn.send(&play::Disconnect(&reason)).await?;
                return conn.close().await;
            }
        };

        let (mut reader, mut writer) = conn.split();
        let (stop, stopped) = oneshot::channel::<()>();
        let name = profile.name.clone();
        let mut writer_task = tokio::spawn(async move {
            let stopped = async {
                let _ = stopped.await;
            };
            match writer
                .pump_outbound_until(&mut outbound_rx, WRITE_BATCH, limit, stopped)
                .await
            {
                Pumped::Stopped => return Some(writer),
                Pumped::Drained => {
                    let _ = writer.close().await;
                }
                Pumped::Failed => {}
                Pumped::Overflow(queued) => {
                    log::info!(
                        "disconnecting {name}: {queued} bytes queued (limit {limit}) for a client that stopped reading"
                    );
                }
            }
            None
        });
        let Reconfigure { hold, reply } = tokio::select! {
            result = read_play_packets(&mut reader, &joined.packets, joined.player, &mut phases) => match result? {
                Some(request) => request,
                None => return Ok(()),
            },
            _ = &mut writer_task => return Ok(()),
        };
        let Joined {
            packets, mut done, ..
        } = joined;
        drop(packets);
        let _ = done.wait_for(|done| *done).await;
        let _ = stop.send(());
        let Ok(Some(writer)) = writer_task.await else {
            return Ok(());
        };
        conn = Connection::rejoin(reader, writer);
        conn.send(&play::StartConfiguration).await?;
        let _ = reply.send(true);
        loop {
            let payload = conn
                .read_packet()
                .await?
                .context("connection closed while leaving play")?;
            if play::Serverbound::decode(payload)? == play::Serverbound::ConfigurationAcknowledged {
                break;
            }
        }
        conn.trace_state(ConnState::Configuration);
        if hold {
            hold_configuration(&mut conn, &mut phases, &mut client).await?;
        }
        synchronize(context, &mut conn, &mut client).await?;
        finish_configuration(&mut conn, &mut client).await?;
    }
}

/// A [`PhaseRequest::Configure`] taken apart.
struct Reconfigure {
    hold: bool,
    reply: oneshot::Sender<bool>,
}

/// Forwards play packets to the player's actor until a request to
/// configure arrives, or the client closes the connection (`None`).
async fn read_play_packets(
    reader: &mut ConnectionReader,
    packets: &mpsc::Sender<play::Serverbound>,
    player: PlayerId,
    phases: &mut mpsc::UnboundedReceiver<PhaseRequest>,
) -> Result<Option<Reconfigure>> {
    loop {
        let payload = tokio::select! {
            biased;
            Some(request) = phases.recv() => match request {
                PhaseRequest::Configure { hold, reply } => {
                    return Ok(Some(Reconfigure { hold, reply }));
                }
                PhaseRequest::Release { reply } => {
                    let _ = reply.send(false);
                    continue;
                }
            },
            payload = reader.read_packet() => match payload? {
                Some(payload) => payload,
                None => return Ok(None),
            },
        };
        let packet = play::Serverbound::decode(payload)?;
        match packet {
            play::Serverbound::Ignored => continue,
            play::Serverbound::ConfigurationAcknowledged => {
                bail!("client acknowledged a configuration that was not requested")
            }
            _ => {}
        }
        if packets.send(packet).await.is_err() {
            bail!("player {player} is no longer in the world");
        }
    }
}

const CONFIGURATION_KEEP_ALIVE: Duration = Duration::from_secs(10);

/// Keeps a connection in configuration until a [`PhaseRequest::Release`].
async fn hold_configuration(
    conn: &mut Connection,
    phases: &mut mpsc::UnboundedReceiver<PhaseRequest>,
    client: &mut ClientDetails,
) -> Result<()> {
    let mut keep_alive = tokio::time::interval(CONFIGURATION_KEEP_ALIVE);
    keep_alive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let started = tokio::time::Instant::now();
    loop {
        tokio::select! {
            biased;
            Some(request) = phases.recv() => match request {
                PhaseRequest::Release { reply } => {
                    let _ = reply.send(true);
                    return Ok(());
                }
                PhaseRequest::Configure { reply, .. } => {
                    let _ = reply.send(false);
                }
            },
            packet = next_configuration_packet(conn) => client.absorb(packet?),
            now = keep_alive.tick() => {
                let id = now.duration_since(started).as_millis() as i64;
                conn.send(&configuration::KeepAlive(id)).await?;
            }
        }
    }
}

async fn disconnect(mut conn: Connection, reason: impl Into<TextComponent>) -> Result<()> {
    conn.send(&LoginDisconnect::new(&reason.into())).await?;
    conn.close().await
}

#[cfg(test)]
mod log_ip_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn the_peer_address_is_logged_only_when_log_ips_is_on() {
        let err = anyhow::anyhow!("boom");
        let addr: SocketAddr = "203.0.113.9:5000".parse().unwrap();
        assert!(disconnect_message(Some(addr), &err).contains("203.0.113.9"));
        let hidden = disconnect_message(None, &err);
        assert!(!hidden.contains("203.0.113.9") && hidden.contains("boom"));
    }
}

#[cfg(test)]
mod offline_name_tests {
    use super::valid_offline_name;

    #[tuff_core::watchdog]
    #[test]
    fn names_are_at_most_sixteen_graphic_ascii_characters() {
        assert!(valid_offline_name("Steve"));
        assert!(valid_offline_name("abcdefghijklmnop"));
        assert!(!valid_offline_name("abcdefghijklmnopq"));
        assert!(!valid_offline_name(""));
        assert!(!valid_offline_name("a b"));
    }
}
