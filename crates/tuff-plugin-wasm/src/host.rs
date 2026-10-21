//! The host actor: owns every loaded plugin, its sandbox and everything
//! it registered, and is the only path into guest code. Loading,
//! unloading, commands, events and timers are messages on one bounded
//! channel, handled in order, so an unload can never race a call into
//! the same plugin and nothing a removed plugin registered outlives it.
//!
//! Guest code runs on the blocking pool with the sandbox handed off and
//! back; a fault (trap, fuel exhausted, ABI misuse, panic) drops the
//! sandbox and unloads the plugin, and the server carries on.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use crate::wit::runtime::Engine;
use tokio::sync::{mpsc, oneshot, watch};
use tuff_core::world::Shared;
use tuff_core::world::commands::text;
use tuff_core::world::messages::CoordinatorMessage;

use crate::abi;
use crate::caps::{Grants, Policy};
use crate::files::{Access, Files};
use crate::manifest::{self, MAX_MODULE_BYTES, Manifest};
use crate::sandbox::{self, ClickFilter, Effects, Fault, Limits, Player, Registration, Sandbox};
use crate::storage::{self, Storage};
use crate::wit::bindings::capability_types::IoEvent;
use crate::wit::bindings::types::{ClickKind, Event, EventKinds};
use crate::world::{self, Env};

mod dispatch;
mod lifecycle;
mod loading;
mod messaging;
mod routes;
mod settings;

/// Requests queued before the host stops taking more. Bounded so a flood
/// of commands or admin requests backs up into their senders.
const QUEUE: usize = 256;
/// Timers one plugin may have pending.
pub const MAX_PENDING_TIMERS: usize = 1024;
/// Server ticks between writes of changed plugin stores.
const FLUSH_TICKS: u64 = 100;

/// What sessions read without asking the actor: the command names and
/// event kinds loaded plugins claim.
#[derive(Debug, Default)]
pub struct Index {
    pub plugins: usize,
    pub commands: HashMap<String, String>,
    pub tuff_subcommands: HashMap<String, String>,
    pub aliases: HashMap<String, (String, String)>,
    pub events: EventKinds,
    pub clicks: Vec<ClickFilter>,
    pub items: Vec<crate::sandbox::ItemFilter>,
    pub channels: Vec<String>,
    pub inventory: Vec<crate::wit::bindings::types::InventoryFilter>,
    pub state: Vec<crate::wit::bindings::types::StateFilter>,
    pub entity_filters: Vec<crate::menus::EntityFilter>,
    pub command_filters: Vec<crate::line_filters::CommandFilter>,
    pub chat_filters: Vec<crate::line_filters::ChatFilter>,
    pub attack_filters: Vec<crate::combat::AttackFilter>,
    pub damage_filters: Vec<crate::combat::DamageFilter>,
    /// Block ids loaded plugins watch for bodies inside.
    pub watched: Vec<String>,
    pub regions: Vec<crate::wit::bindings::types::RegionWatch>,
    /// Each loaded plugin's requested capabilities as `(kind, entry)`.
    pub requests: BTreeMap<String, Vec<(String, String)>>,
    /// Block rules and teams.
    pub guard: Arc<crate::guard::Published>,
    /// Interface to its providers, each with the method keys it serves.
    pub providers: BTreeMap<String, Vec<(String, Vec<u64>)>>,
    /// Loaded plugin ids to version text.
    pub versions: BTreeMap<String, String>,
}

impl Index {
    /// The plugin that runs `line` and the line it receives: an alias
    /// root is rewritten to its target, a `tuff <sub> ...` line goes to
    /// the plugin owning `<sub>`.
    pub fn resolve(&self, line: &str) -> Option<(String, String)> {
        let line = line.trim_start();
        let (root, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        if let Some((owner, target)) = self.aliases.get(root) {
            let rest = rest.trim_start();
            let canonical = if rest.is_empty() {
                target.clone()
            } else {
                format!("{target} {rest}")
            };
            return Some((owner.clone(), canonical));
        }
        if let Some(owner) = self.commands.get(root) {
            return Some((owner.clone(), line.to_owned()));
        }
        if root == "tuff" {
            let sub = rest.split_whitespace().next()?;
            let owner = self.tuff_subcommands.get(sub)?;
            return Some((owner.clone(), line.to_owned()));
        }
        None
    }
}

/// One loaded plugin, as `/tuff plugin list` shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub id: String,
    pub version: manifest::Version,
    pub description: String,
    pub file: PathBuf,
    pub commands: Vec<String>,
    pub depends: Vec<String>,
    pub permissions: Vec<String>,
}

/// What a plugin command produced.
#[derive(Debug, PartialEq, Eq)]
pub struct CommandOutcome {
    /// The command's result, or why the plugin failed.
    pub result: Result<i32, String>,
    pub replies: Vec<String>,
}

pub use crate::grants_edit::{GrantChange, GrantReport};

enum Request {
    Load {
        file: String,
        reply: oneshot::Sender<Result<String, String>>,
    },
    Unload {
        id: String,
        reply: oneshot::Sender<Result<String, String>>,
    },
    Reload {
        id: String,
        reply: oneshot::Sender<Result<String, String>>,
    },
    Purge {
        id: String,
        reply: oneshot::Sender<Result<String, String>>,
    },
    Suggest {
        player: Player,
        line: String,
        reply: oneshot::Sender<Option<Vec<String>>>,
    },
    Stored {
        id: String,
        reply: oneshot::Sender<Option<Storage>>,
    },
    GrantReport {
        id: String,
        reply: oneshot::Sender<Result<GrantReport, String>>,
    },
    GrantChange {
        id: String,
        change: GrantChange,
        reply: oneshot::Sender<Result<String, String>>,
    },
    Policy {
        id: String,
        reply: oneshot::Sender<Option<watch::Receiver<Arc<Policy>>>>,
    },
    List {
        reply: oneshot::Sender<Vec<Summary>>,
    },
    Command {
        player: Player,
        line: String,
        reply: oneshot::Sender<Option<CommandOutcome>>,
    },
    Event {
        event: Event,
        player: Option<Player>,
    },
    Detailed {
        event: Event,
        player: Option<Player>,
        detail: crate::guard::Detail,
    },
    Login {
        player: Player,
        address: String,
        reply: std::sync::mpsc::SyncSender<Option<String>>,
    },
    Sync {
        reply: oneshot::Sender<()>,
    },
    Route(crate::routes::Call),
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

struct Startup {
    inbox: mpsc::Receiver<Request>,
    index: watch::Sender<Arc<Index>>,
    menus: Arc<crate::menus::Menus>,
}

/// The world service that runs the host actor. Plugin code finds it with
/// `shared.services.get::<WasmHost>()`.
pub struct WasmHost {
    requests: mpsc::Sender<Request>,
    index: watch::Receiver<Arc<Index>>,
    startup: Mutex<Option<Startup>>,
    directory: Option<PathBuf>,
    limits: Limits,
    menus: Arc<crate::menus::Menus>,
}

impl WasmHost {
    pub fn new(directory: Option<PathBuf>, limits: Limits) -> Self {
        let (requests, inbox) = mpsc::channel(QUEUE);
        let (index, index_rx) = watch::channel(Arc::new(Index::default()));
        let menus = Arc::<crate::menus::Menus>::default();
        Self {
            requests,
            index: index_rx,
            startup: Mutex::new(Some(Startup {
                inbox,
                index,
                menus: menus.clone(),
            })),
            directory,
            limits,
            menus,
        }
    }

    /// The menus players have open.
    pub fn menus(&self) -> &crate::menus::Menus {
        &self.menus
    }

    /// Whether a loaded plugin runs the command `line`.
    pub fn claims(&self, line: &str) -> bool {
        self.index.borrow().resolve(line).is_some()
    }

    pub fn command_outcome(&self, line: &str) -> crate::line_filters::Outcome {
        crate::line_filters::command_outcome(&self.index.borrow().command_filters, line)
    }

    pub fn chat_outcome(&self, message: &str) -> crate::line_filters::Outcome {
        crate::line_filters::chat_outcome(&self.index.borrow().chat_filters, message)
    }

    /// Whether a loaded plugin listens for any of `kinds`.
    pub fn listens(&self, kinds: EventKinds) -> bool {
        self.index.borrow().events.intersects(kinds)
    }

    /// Whether a loaded plugin's click filter for `kind` matches a click
    /// with `item` (an item id) held, whose `custom_data` has `has_tag`.
    pub fn intercepts(
        &self,
        kind: ClickKind,
        item: &str,
        has_tag: impl Fn(&str) -> bool,
        scoped: impl Fn(&crate::combat::Scope) -> bool,
    ) -> bool {
        self.matching_clicks(kind, item, has_tag, scoped)
            .iter()
            .any(Option::is_none)
    }

    /// The block constraints of the click filters for `kind` that match
    /// `item` and `has_tag`: none means that filter takes any block.
    pub fn matching_clicks(
        &self,
        kind: ClickKind,
        item: &str,
        has_tag: impl Fn(&str) -> bool,
        scoped: impl Fn(&crate::combat::Scope) -> bool,
    ) -> Vec<Option<String>> {
        self.index
            .borrow()
            .clicks
            .iter()
            .filter(|filter| {
                filter.kind == kind
                    && filter.item.as_deref().is_none_or(|wanted| wanted == item)
                    && filter.tag.as_deref().is_none_or(&has_tag)
                    && scoped(&filter.scope)
            })
            .map(|filter| filter.block.clone())
            .collect()
    }

    /// Whether a loaded plugin watches any block with `watch-block`.
    pub fn watching(&self) -> bool {
        !self.index.borrow().watched.is_empty()
    }

    /// How many plugins are loaded.
    /// What sessions and commands may read without asking the actor.
    pub fn snapshot(&self) -> Arc<Index> {
        self.index.borrow().clone()
    }

    pub fn loaded(&self) -> usize {
        self.index.borrow().plugins
    }

    async fn ask<T>(&self, request: impl FnOnce(oneshot::Sender<T>) -> Request) -> Option<T> {
        let (reply, answer) = oneshot::channel();
        self.requests.send(request(reply)).await.ok()?;
        answer.await.ok()
    }

    fn stopped() -> String {
        "the plugin host is not running".to_owned()
    }

    /// Loads `file` from the plugin directory.
    pub async fn load(&self, file: &str) -> Result<String, String> {
        let file = file.to_owned();
        self.ask(|reply| Request::Load { file, reply })
            .await
            .unwrap_or_else(|| Err(Self::stopped()))
    }

    pub async fn unload(&self, id: &str) -> Result<String, String> {
        let id = id.to_owned();
        self.ask(|reply| Request::Unload { id, reply })
            .await
            .unwrap_or_else(|| Err(Self::stopped()))
    }

    /// Re-reads plugin `id`'s file and swaps it in; the old one keeps
    /// running if the new one does not load.
    pub async fn reload(&self, id: &str) -> Result<String, String> {
        let id = id.to_owned();
        self.ask(|reply| Request::Reload { id, reply })
            .await
            .unwrap_or_else(|| Err(Self::stopped()))
    }

    /// Deletes everything plugin `id` stored, loaded or not.
    pub async fn purge(&self, id: &str) -> Result<String, String> {
        let id = id.to_owned();
        self.ask(|reply| Request::Purge { id, reply })
            .await
            .unwrap_or_else(|| Err(Self::stopped()))
    }

    /// What plugin `id` has stored, as the host keeps it; `None` if it has
    /// stored nothing since the host started and has no file.
    pub async fn stored(&self, id: &str) -> Option<Storage> {
        let id = id.to_owned();
        self.ask(|reply| Request::Stored { id, reply })
            .await
            .flatten()
    }

    /// Requested versus granted capabilities of loaded plugin `id`.
    pub async fn grants(&self, id: &str) -> Result<GrantReport, String> {
        let id = id.to_owned();
        self.ask(|reply| Request::GrantReport { id, reply })
            .await
            .unwrap_or_else(|| Err(Self::stopped()))
    }

    /// Grants or revokes capabilities of loaded plugin `id`, durably and
    /// at once.
    pub async fn change_grant(&self, id: &str, change: GrantChange) -> Result<String, String> {
        let id = id.to_owned();
        self.ask(|reply| Request::GrantChange { id, change, reply })
            .await
            .unwrap_or_else(|| Err(Self::stopped()))
    }

    /// A live view of loaded plugin `id`'s policy: it changes the moment
    /// the owner grants or revokes, so executors drop what it no longer
    /// allows. It closes when the plugin unloads.
    pub async fn policy(&self, id: &str) -> Option<watch::Receiver<Arc<Policy>>> {
        let id = id.to_owned();
        self.ask(|reply| Request::Policy { id, reply })
            .await
            .flatten()
    }

    pub async fn list(&self) -> Vec<Summary> {
        self.ask(|reply| Request::List { reply })
            .await
            .unwrap_or_default()
    }

    /// Runs `line` (no slash) for `player` if a plugin owns its command;
    /// `None` leaves it to the server.
    pub async fn command(&self, player: Player, line: String) -> Option<CommandOutcome> {
        self.ask(|reply| Request::Command {
            player,
            line,
            reply,
        })
        .await
        .flatten()
    }

    /// Completions the owning plugin offers for the argument at the end of
    /// `line`; `None` when no plugin claims it.
    pub async fn suggest(&self, player: Player, line: String) -> Option<Vec<String>> {
        if !self.claims(&line) {
            return None;
        }
        self.ask(|reply| Request::Suggest {
            player,
            line,
            reply,
        })
        .await
        .flatten()
    }

    /// Queues `event`, acting for `player`, without waiting; dropped if
    /// the host is backed up, as events are advisory.
    pub fn event(&self, event: Event, player: Option<Player>) {
        if self.listens(sandbox::kind_of(&event)) {
            let _ = self.requests.try_send(Request::Event { event, player });
        }
    }

    /// [`Self::event`] with what the `acting-*` imports answer while
    /// plugins handle it.
    pub fn detailed_event(
        &self,
        event: Event,
        player: Option<Player>,
        detail: crate::guard::Detail,
    ) {
        if self.listens(sandbox::kind_of(&event)) {
            let _ = self.requests.try_send(Request::Detailed {
                event,
                player,
                detail,
            });
        }
    }

    /// Asks plugins listening for logins whether to refuse this one,
    /// waiting at most `timeout`. `None` admits (or, with nothing
    /// listening, costs nothing); a timeout counts as `unanswered`.
    pub fn decide_login(
        &self,
        player: Player,
        address: String,
        timeout: std::time::Duration,
        unanswered: Option<String>,
    ) -> Option<String> {
        if !self.listens(EventKinds::LOGIN) {
            return None;
        }
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        let request = Request::Login {
            player,
            address,
            reply,
        };
        if self.requests.try_send(request).is_err() {
            return unanswered;
        }
        answer.recv_timeout(timeout).unwrap_or(unanswered)
    }

    /// Player `player` left: plugins listening hear it, and every plugin
    /// drops what it kept for them. Waits for queue room so the state is
    /// always dropped.
    pub async fn left(&self, player: Player) {
        if self.loaded() > 0 {
            let _ = self
                .requests
                .send(Request::Event {
                    event: Event::Leave(player.to_wit()),
                    player: Some(player),
                })
                .await;
        }
    }

    /// Resolves once every earlier request is handled and every timer due
    /// by the current server tick has fired.
    pub async fn sync(&self) {
        self.ask(|reply| Request::Sync { reply }).await;
    }
}

impl tuff_core::world::services::WorldService for WasmHost {
    fn start(&self, shared: &Arc<Shared>) {
        let Some(startup) = self.startup.lock().unwrap().take() else {
            return;
        };
        let (io_queue, io_events) = mpsc::unbounded_channel();
        let actor = Actor {
            shared: shared.clone(),
            engine: sandbox::engine(),
            directory: self.directory.clone(),
            enabled: true,
            limits: self.limits,
            plugins: BTreeMap::new(),
            commands: HashMap::new(),
            tuff_subcommands: HashMap::new(),
            aliases: HashMap::new(),
            timers: BTreeMap::new(),
            timer_seq: 0,
            generation: 0,
            index: startup.index,
            menus: startup.menus,
            stores: HashMap::new(),
            traffic: Default::default(),
            io_queue,
            resolver: Arc::new(crate::caps::resolve::SystemResolver),
            hops: 0,
            flushed_at: 0,
            guard_serial: 0,
            grants: None,
            file_done: mpsc::unbounded_channel(),
            routes: routes::RouteState::new(self.requests.downgrade()),
        };
        tokio::spawn(actor.run(startup.inbox, io_events));
    }

    fn settled(&self, _tick: u64) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(self.sync())
    }

    fn flush(&self) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {
            self.ask(|reply| Request::Shutdown { reply }).await;
        })
    }
}

struct Loaded {
    policy: watch::Sender<Arc<Policy>>,
    manifest: Manifest,
    file: PathBuf,
    generation: u64,
    sandbox: Option<Box<Sandbox>>,
    registration: Registration,
    files: Arc<Files>,
    /// The last value of each declared setting the plugin was told about.
    settings: HashMap<String, tuff_core::config::Value>,
    guard: crate::guard::Owned,
}

/// A finished file operation on its way to the plugin that asked.
struct FileDone {
    plugin: String,
    generation: u64,
    done: crate::files::Done,
}

struct Timer {
    plugin: String,
    generation: u64,
    token: u32,
}

/// A plugin's persistent store as the host keeps it, by plugin id.
struct Stored {
    storage: Storage,
    dirty: bool,
}

struct Actor {
    file_done: (
        mpsc::UnboundedSender<FileDone>,
        mpsc::UnboundedReceiver<FileDone>,
    ),
    grants: Option<Grants>,
    routes: routes::RouteState,
    shared: Arc<Shared>,
    engine: Engine,
    /// Overrides the directory setting (tests).
    directory: Option<PathBuf>,
    enabled: bool,
    limits: Limits,
    plugins: BTreeMap<String, Loaded>,
    /// Command name to the plugin that owns it.
    commands: HashMap<String, String>,
    /// `/tuff` subcommand name to the plugin that owns it.
    tuff_subcommands: HashMap<String, String>,
    /// Alias root to its owner and the line it rewrites to.
    aliases: HashMap<String, (String, String)>,
    /// `(due server tick, sequence)` to the timer.
    timers: BTreeMap<(u64, u64), Timer>,
    timer_seq: u64,
    generation: u64,
    index: watch::Sender<Arc<Index>>,
    menus: Arc<crate::menus::Menus>,
    /// Kept across unloads, so a plugin finds its data again.
    stores: HashMap<String, Stored>,
    traffic: messaging::Traffic,
    io_queue: mpsc::UnboundedSender<crate::net::Delivery>,
    resolver: Arc<dyn crate::caps::resolve::Resolver>,
    /// How deep the message being delivered is.
    hops: usize,
    flushed_at: u64,
    /// The serial of the latest block rule added.
    guard_serial: u64,
}

/// Runs `f` on the blocking pool with `sandbox` handed over; a panic
/// becomes a fault and loses the sandbox.
async fn on_sandbox<R: Send + 'static>(
    mut sandbox: Box<Sandbox>,
    f: impl FnOnce(&mut Sandbox) -> Result<R, Fault> + Send + 'static,
) -> (Option<Box<Sandbox>>, Result<R, Fault>) {
    let joined = tokio::task::spawn_blocking(move || {
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| f(&mut sandbox)));
        match result {
            Ok(result) => (Some(sandbox), result),
            Err(_) => (None, Err(Fault("the plugin host panicked".into()))),
        }
    })
    .await;
    joined.unwrap_or_else(|_| (None, Err(Fault("the plugin call was cancelled".into()))))
}

/// Whether `file` names a module directly inside the plugin directory.
fn module_name(file: &str) -> Result<String, String> {
    let file = file.trim();
    if file.is_empty()
        || file.starts_with('.')
        || file.contains(['/', '\\', ':', '\0'])
        || file.len() > 128
    {
        return Err(format!(
            "{file:?} is not a file name in the plugin directory"
        ));
    }
    Ok(if file.ends_with(".wasm") {
        file.to_owned()
    } else {
        format!("{file}.wasm")
    })
}

async fn read_module(path: &Path) -> Result<Vec<u8>, String> {
    let shown = path.display();
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|err| format!("cannot read {shown}: {err}"))?;
    if meta.len() > MAX_MODULE_BYTES as u64 {
        return Err(format!(
            "{shown} is {} bytes, over the {MAX_MODULE_BYTES} byte limit",
            meta.len()
        ));
    }
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|err| format!("cannot read {shown}: {err}"))?;
    if bytes.len() > MAX_MODULE_BYTES {
        return Err(format!("{shown} grew past the size limit while being read"));
    }
    Ok(bytes)
}

impl Actor {
    async fn run(
        mut self,
        mut inbox: mpsc::Receiver<Request>,
        mut io_events: mpsc::UnboundedReceiver<crate::net::Delivery>,
    ) {
        self.register_base().await;
        self.enabled = self.setting();
        if self.enabled {
            self.scan().await;
        }
        let mut ticks = self.shared.server_ticks.clone();
        let mut ticking = true;
        let mut config = self.shared.config.subscribe();
        let mut configurable = true;
        loop {
            tokio::select! {
                Some(finished) = self.file_done.1.recv() => {
                    self.file_finished(finished).await;
                }
                request = inbox.recv() => match request {
                    None => break,
                    Some(Request::Shutdown { reply }) => {
                        self.unload_all().await;
                        let _ = reply.send(());
                        break;
                    }
                    Some(request) => {
                        self.handle(request).await;
                        self.deliver().await;
                    }
                },
                Some(delivery) = io_events.recv() => {
                    self.deliver_io(delivery).await;
                    self.deliver().await;
                },
                changed = ticks.changed(), if ticking => match changed {
                    Ok(()) => self.fire_due().await,
                    Err(_) => ticking = false,
                },
                changed = config.changed(), if configurable => match changed {
                    Ok(()) => {
                        self.reconfigure().await;
                        self.settings_changed().await;
                        self.deliver().await;
                    }
                    Err(_) => configurable = false,
                },
            }
        }
        self.unload_all().await;
        self.unregister_base().await;
    }

    fn setting(&self) -> bool {
        self.shared
            .config
            .get_bool(crate::ENABLED_KEY)
            .unwrap_or(true)
    }

    /// Where modules are read from: the plugin's override, else the
    /// setting for a world on disk.
    fn directory(&self) -> Option<PathBuf> {
        if let Some(directory) = &self.directory {
            return Some(directory.clone());
        }
        self.shared.world_path.as_ref()?;
        let setting = self.shared.config.get_string(crate::DIRECTORY_KEY);
        let directory = setting
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .unwrap_or(crate::DEFAULT_DIRECTORY);
        Some(PathBuf::from(directory))
    }

    /// Turning runtime plugins off unloads them all; turning them back on
    /// loads the plugin directory again.
    async fn reconfigure(&mut self) {
        let enabled = self.setting();
        if enabled == self.enabled {
            return;
        }
        self.enabled = enabled;
        if enabled {
            log::info!("runtime plugins turned on");
            self.scan().await;
        } else {
            log::info!("runtime plugins turned off; unloading them");
            self.unload_all().await;
        }
    }

    fn turned_off(&self) -> Result<(), String> {
        if self.enabled {
            Ok(())
        } else {
            Err(format!(
                "runtime plugins are turned off ({} is false)",
                crate::ENABLED_KEY
            ))
        }
    }

    async fn handle(&mut self, request: Request) {
        self.reconfigure().await;
        match request {
            Request::Load { file, reply } => {
                let result = self.load_file(&file).await;
                let _ = reply.send(result);
            }
            Request::Unload { id, reply } => {
                let result = self.unload(&id).await;
                let _ = reply.send(result);
            }
            Request::Reload { id, reply } => {
                let result = self.reload(&id).await;
                let _ = reply.send(result);
            }
            Request::Purge { id, reply } => {
                let result = self.purge(&id).await;
                let _ = reply.send(result);
            }
            Request::Stored { id, reply } => {
                let stored = match self.stores.get(&id) {
                    Some(stored) => Some(stored.storage.clone()),
                    None => self.read_store(&id).await.ok().filter(|s| !s.is_empty()),
                };
                let _ = reply.send(stored);
            }
            Request::GrantReport { id, reply } => {
                let result = self.grant_report(id).await;
                let _ = reply.send(result);
            }
            Request::GrantChange { id, change, reply } => {
                let result = self.change_grant(id, change).await;
                let _ = reply.send(result);
            }
            Request::Policy { id, reply } => {
                let _ = reply.send(self.plugins.get(&id).map(|p| p.policy.subscribe()));
            }
            Request::List { reply } => {
                let _ = reply.send(self.summaries());
            }
            Request::Command {
                player,
                line,
                reply,
            } => {
                let outcome = self.command(player, line).await;
                let _ = reply.send(outcome);
            }
            Request::Suggest {
                player,
                line,
                reply,
            } => {
                let found = self.suggest(player, line).await;
                let _ = reply.send(found);
            }
            Request::Event { event, player } => self.event(event, player).await,
            Request::Detailed {
                event,
                player,
                detail,
            } => self.detailed_event(event, player, detail).await,
            Request::Login {
                player,
                address,
                reply,
            } => {
                let refusal = self.login(player, address).await;
                let _ = reply.send(refusal);
            }
            Request::Sync { reply } => {
                self.fire_due().await;
                let _ = reply.send(());
            }
            Request::Route(call) => self.route(call).await,
            Request::Shutdown { reply } => {
                let _ = reply.send(());
            }
        }
    }

    fn publish(&self) {
        if let Some(claims) = self
            .shared
            .services
            .get::<tuff_core::world::contact::PortalClaims>()
        {
            claims.set(
                self.plugins
                    .values()
                    .flat_map(|plugin| plugin.registration.watched.iter().cloned())
                    .collect(),
            );
        }
        let index = Index {
            plugins: self.plugins.len(),
            commands: self.commands.clone(),
            tuff_subcommands: self.tuff_subcommands.clone(),
            aliases: self.aliases.clone(),
            events: self
                .plugins
                .values()
                .fold(EventKinds::empty(), |events, plugin| {
                    events | plugin.registration.events
                }),
            clicks: self
                .plugins
                .values()
                .flat_map(|plugin| plugin.registration.clicks.iter().cloned())
                .collect(),
            attack_filters: self
                .plugins
                .values()
                .flat_map(|plugin| plugin.registration.attack_filters.iter().cloned())
                .collect(),
            damage_filters: self
                .plugins
                .values()
                .flat_map(|plugin| plugin.registration.damage_filters.iter().cloned())
                .collect(),
            inventory: self
                .plugins
                .values()
                .flat_map(|plugin| plugin.registration.inventory.iter().cloned())
                .collect(),
            state: self
                .plugins
                .values()
                .flat_map(|plugin| plugin.registration.state.iter().cloned())
                .collect(),
            channels: self
                .plugins
                .values()
                .flat_map(|plugin| plugin.registration.channels.iter().cloned())
                .collect(),
            items: self
                .plugins
                .values()
                .flat_map(|plugin| plugin.registration.items.iter().cloned())
                .collect(),
            entity_filters: self
                .plugins
                .values()
                .flat_map(|plugin| plugin.registration.entity_filters.iter().cloned())
                .collect(),
            command_filters: self
                .plugins
                .values()
                .flat_map(|plugin| {
                    let may_rewrite = plugin
                        .policy
                        .borrow()
                        .check_group(crate::caps::Kind::PlayerWrite)
                        .is_ok();
                    plugin
                        .registration
                        .command_filters
                        .iter()
                        .filter(move |filter| may_rewrite || !filter.action.is_rewrite())
                        .cloned()
                })
                .collect(),
            chat_filters: self
                .plugins
                .values()
                .flat_map(|plugin| {
                    let may_rewrite = plugin
                        .policy
                        .borrow()
                        .check_group(crate::caps::Kind::PlayerWrite)
                        .is_ok();
                    plugin
                        .registration
                        .chat_filters
                        .iter()
                        .filter(move |filter| may_rewrite || !filter.action.is_rewrite())
                        .cloned()
                })
                .collect(),
            regions: self
                .plugins
                .values()
                .flat_map(|plugin| plugin.registration.regions.iter().cloned())
                .collect(),
            watched: {
                let mut watched: Vec<String> = self
                    .plugins
                    .values()
                    .flat_map(|plugin| plugin.registration.watched.iter().cloned())
                    .collect();
                watched.sort_unstable();
                watched.dedup();
                watched
            },
            requests: self
                .plugins
                .iter()
                .map(|(id, plugin)| {
                    let entries = plugin.manifest.capabilities.iter().map(|request| {
                        let text = request.entry.to_string();
                        let (kind, entry) = text.split_once(' ').unwrap_or((&text, ""));
                        (kind.to_owned(), entry.to_owned())
                    });
                    (id.clone(), entries.collect())
                })
                .collect(),
            providers: {
                let mut providers: BTreeMap<String, Vec<(String, Vec<u64>)>> = BTreeMap::new();
                for plugin in self.plugins.values() {
                    for (interface, methods) in &plugin.registration.provides {
                        providers
                            .entry(interface.clone())
                            .or_default()
                            .push((plugin.manifest.id.clone(), methods.clone()));
                    }
                }
                providers
            },
            versions: self
                .plugins
                .values()
                .map(|plugin| {
                    (
                        plugin.manifest.id.clone(),
                        plugin.manifest.version.to_string(),
                    )
                })
                .collect(),
            guard: Arc::new(crate::guard::Published::build(
                self.plugins.values().map(|plugin| &plugin.guard),
                self.guard_serial,
            )),
        };
        self.shared.commands.client_roots().set(
            self.commands.keys().chain(self.aliases.keys()).cloned(),
            self.tuff_subcommands.keys().cloned(),
            self.plugins
                .values()
                .flat_map(|plugin| plugin.registration.commands.iter())
                .map(client_tree)
                .collect(),
        );
        self.index.send_replace(Arc::new(index));
    }

    fn summaries(&self) -> Vec<Summary> {
        self.plugins
            .values()
            .map(|plugin| Summary {
                id: plugin.manifest.id.clone(),
                version: plugin.manifest.version,
                description: plugin.manifest.description.clone(),
                file: plugin.file.clone(),
                commands: plugin
                    .registration
                    .command_names()
                    .map(str::to_owned)
                    .collect(),
                depends: plugin
                    .manifest
                    .depends
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                permissions: plugin
                    .registration
                    .permissions
                    .iter()
                    .map(|permission| permission.node.clone())
                    .collect(),
            })
            .collect()
    }

    /// Runs `f` on plugin `id`'s sandbox. `None` if it is not loaded. A
    /// fault leaves the sandbox out; the caller unloads the plugin.
    async fn call<R: Send + 'static>(
        &mut self,
        id: &str,
        f: impl FnOnce(&mut Sandbox) -> Result<R, Fault> + Send + 'static,
    ) -> Option<Result<R, Fault>> {
        let sandbox = self.plugins.get_mut(id)?.sandbox.take()?;
        let (sandbox, result) = on_sandbox(sandbox, f).await;
        if result.is_ok()
            && let Some(plugin) = self.plugins.get_mut(id)
        {
            plugin.sandbox = sandbox;
        }
        Some(result)
    }

    /// Carries out a call's effects and returns its replies.
    async fn apply(&mut self, id: &str, effects: Effects) -> Vec<String> {
        self.finish_routes(id, effects.responses);
        if !effects.guard.is_empty()
            && let Some(plugin) = self.plugins.get_mut(id)
            && plugin.guard.apply(effects.guard, &mut self.guard_serial)
        {
            self.publish();
        }
        if !effects.stored.is_empty()
            && let Some(stored) = self.stores.get_mut(id)
        {
            for (key, value) in effects.stored {
                stored.storage.set(&key, value, self.limits.storage);
            }
            stored.dirty = true;
        }
        for op in effects.menus {
            self.menus.apply(op);
        }
        if !effects.blocks.is_empty() {
            world::write(self.shared.clone(), effects.blocks).await;
        }
        if !effects.biomes.is_empty() {
            world::write_biomes(self.shared.clone(), effects.biomes).await;
        }
        if !effects.teleports.is_empty() {
            crate::portal::apply(&self.shared, effects.teleports).await;
        }
        if !effects.entity_ops.is_empty() {
            crate::entities::apply(&self.shared, effects.entity_ops).await;
        }
        for dimension in effects.discards {
            tokio::spawn(crate::dimensions::discard(self.shared.clone(), dimension));
        }
        let player_write = self.plugins.get(id).is_some_and(|plugin| {
            plugin
                .policy
                .borrow()
                .check_group(crate::caps::Kind::PlayerWrite)
                .is_ok()
        });
        if player_write {
            for (uuid, ops) in by_player(effects.inventory_ops) {
                let _ = self
                    .shared
                    .coordinator
                    .send(CoordinatorMessage::PluginInventory { uuid, ops })
                    .await;
            }
        }
        crate::particles::apply(&self.shared, effects.particles, id).await;
        if player_write && !effects.player_ops.is_empty() {
            crate::player_state::apply(&self.shared, effects.player_ops).await;
        }
        for op in effects.advancements {
            let _ = self
                .shared
                .coordinator
                .send(CoordinatorMessage::PluginAdvancement {
                    uuid: op.player,
                    grant: op.grant,
                    advancement: op.advancement,
                    criterion: op.criterion,
                })
                .await;
        }
        if !effects.displays.is_empty() {
            crate::display::apply(&self.shared, id, effects.displays).await;
        }
        for message in effects.messages {
            self.enqueue(id, message);
        }
        for (request, outcome) in effects.answers {
            self.answer(id, request, outcome);
        }
        for (uuid, message) in effects.tells {
            let _ = self
                .shared
                .coordinator
                .send(CoordinatorMessage::Tell {
                    uuid,
                    message: text(message),
                })
                .await;
        }
        for (uuid, message) in effects.rich_tells {
            let _ = self
                .shared
                .coordinator
                .send(CoordinatorMessage::Tell { uuid, message })
                .await;
        }
        for message in effects.broadcasts {
            let _ = self
                .shared
                .coordinator
                .send(CoordinatorMessage::Announce(text(message)))
                .await;
        }
        if !effects.timers.is_empty() {
            let now = *self.shared.server_ticks.borrow();
            let generation = self.plugins.get(id).map_or(0, |plugin| plugin.generation);
            let pending = self.timers.values().filter(|t| t.plugin == id).count();
            let room = MAX_PENDING_TIMERS.saturating_sub(pending);
            if effects.timers.len() > room {
                log::warn!(
                    "wasm plugin {id} has {MAX_PENDING_TIMERS} timers pending; dropped {}",
                    effects.timers.len() - room
                );
            }
            for (delay, token) in effects.timers.into_iter().take(room) {
                self.timer_seq += 1;
                self.timers.insert(
                    (now + delay.max(1), self.timer_seq),
                    Timer {
                        plugin: id.to_owned(),
                        generation,
                        token,
                    },
                );
            }
        }
        effects.replies
    }
}

/// The ports the server itself listens on: the game port and every HTTP
/// listener, read when a plugin binds.
struct ServerPorts(Arc<Shared>);

impl crate::net::listen::Reserved for ServerPorts {
    fn ports(&self) -> Pin<Box<dyn Future<Output = Vec<u16>> + Send + '_>> {
        Box::pin(async move {
            let mut ports = Vec::new();
            if let Ok(Some(properties)) = self.0.config_store.read_properties() {
                ports.push(properties.server_port());
            }
            let main = self.0.http.addrs(tuff_http::MAIN_LISTENER).await;
            ports.extend(main.iter().map(|addr| addr.port()));
            ports
        })
    }
}

fn client_tree(
    tree: &crate::wit::bindings::types::CommandTree,
) -> tuff_core::world::commands::ClientTree {
    use crate::wit::{bindings::types::NodeKind, tree::client_parser};
    use tuff_core::world::commands::{ClientNode, ClientTree, intern_name};
    ClientTree {
        nodes: tree
            .nodes
            .iter()
            .map(|node| ClientNode {
                name: intern_name(&node.name),
                parser: match &node.kind {
                    NodeKind::Literal => None,
                    NodeKind::Argument(parser) => {
                        let (name, properties) = client_parser(parser);
                        Some((name, properties))
                    }
                },
                children: node.children.clone(),
                executable: node.executes.is_some(),
                redirect: node.redirect,
                permission: node.permission.clone(),
                suggest: node.suggest,
            })
            .collect(),
    }
}

/// Groups a call's inventory edits by player, each player's in call order.
fn by_player<T>(ops: Vec<(u128, T)>) -> Vec<(u128, Vec<T>)> {
    let mut grouped: Vec<(u128, Vec<T>)> = Vec::new();
    for (uuid, op) in ops {
        match grouped.iter_mut().find(|(player, _)| *player == uuid) {
            Some((_, list)) => list.push(op),
            None => grouped.push((uuid, vec![op])),
        }
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn module_names_stay_inside_the_directory() {
        assert_eq!(module_name("greeter").unwrap(), "greeter.wasm");
        assert_eq!(module_name("greeter.wasm").unwrap(), "greeter.wasm");
        for bad in ["", "../x", "a/b", "a\\b", ".hidden", "/etc/passwd", "c:x"] {
            assert!(module_name(bad).is_err(), "{bad:?}");
        }
    }
}
