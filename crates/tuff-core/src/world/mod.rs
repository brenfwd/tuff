//! The persistent, infinite world, split across actors so it scales with
//! cores and players:
//!
//! - [`region::RegionActor`]: one per 32x32-chunk region (one `.mca` file),
//!   owning its chunks, dropped items, and files. Regions tick in parallel.
//! - [`session::PlayerSession`]: one per connected player, owning that
//!   player's state, what its client has seen, and its player data file.
//! - [`manager::RegionManager`]: the directory of running regions, which
//!   starts and stops them as players come and go.
//! - [`coordinator::Coordinator`]: global state (who is online, chat, the
//!   game clock, `level.dat`).
//!
//! Actors only exchange messages (see [`messages`]); immutable context all
//! of them need lives in [`Shared`].

pub mod advancement;
pub mod anti_xray;
pub mod anvil;
pub mod area;
pub mod attributes;
pub mod beacon;
pub mod block_entity;
pub mod block_ledger;
pub mod block_rules;
pub mod border;
pub mod chunk_io;
pub mod clock;
pub mod command_storage;
pub mod commands;
pub mod config_actor;
pub mod conservation;
pub mod contact;
pub mod coordinator;
pub mod crafting;
pub mod custom_dimension;
pub mod death;
pub mod debug_feeds;
pub mod dimension;
pub mod drops;
pub mod effects;
pub mod entity;
pub mod entity_claim;
pub mod entity_slots;
pub mod exploration;
pub mod explosion;
pub mod fake;
pub mod features;
pub mod fluid_api;
pub mod game_event;
pub mod game_rules;
pub mod generator;
pub mod growth;
pub mod heightmap_kind;
pub mod inert_entity;
pub mod inventory;
pub mod inventory_edit;
pub mod item_port;
pub mod leash;
pub mod lighting;
pub mod live;
pub mod login_gate;
pub mod long_tick;
pub mod loom;
pub mod manager;
pub mod menu;
pub mod messages;
pub mod movement_sync;
pub mod nearby;
pub mod orientation;
pub mod physics;
pub mod place_work;
pub mod placement;
pub mod player;
pub mod player_lease;
pub mod predicate;
pub mod pregen;
pub mod raycast;
pub mod recipe_book;
pub mod redstone;
pub mod region;
pub mod region_api;
pub mod save_gate;
pub mod scoreboard;
pub mod services;
pub mod session;
pub mod stations;
pub mod stats;
pub mod stored_entity;
pub mod tick_barrier;
pub mod tick_rate;
pub mod ticks;
pub mod tracking;
pub mod trading;
pub mod world_preset;

pub mod block_contact;
pub mod experience;
pub mod fall;
pub mod freezing;
pub mod items;
pub mod poi;
pub mod projectile;
pub mod waypoints;
pub mod weather;

use crate::admin::ServerPropertiesExt as _;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot, watch};
use tuff_vanilla_data::damage_type::DamageType;
use tuff_vanilla_data::data_registry::{self, DataRegistry};

use crate::auth::GameProfile;
use crate::net::connection::frame;
use crate::protocol::Encode;
use crate::protocol::configuration::ClientInformation;
use crate::protocol::play;
use crate::status::{Favicon, StatusCommand};
use crate::storage::BackendOptions;
use crate::storage::Storage;
use crate::storage::codec::WorldBackendExt as _;
use crate::storage::level::{LevelData, Spawn};
use clock::WorldClocks;
use coordinator::Coordinator;
use dimension::{Dimension, DimensionId, Dimensions};
use game_rules::GameRules;
use manager::RegionManager;
use messages::{CoordinatorMessage, ManagerMessage, Packets};
use player::Difficulty;
use tuff_item::text::TextComponent;
use tuff_storage::admin::ServerProperties;
use tuff_storage::backend::WorldBackend;
use tuff_storage::config::ConfigStore;
use tuff_storage::vanilla::config::VanillaConfig;
use weather::Weather;

pub use messages::{Joined, PlayerId};

/// Packets at or above this many bytes are zlib-compressed, as in vanilla.
pub const COMPRESSION_THRESHOLD: usize = 256;

pub const DEFAULT_VIEW_DISTANCE: i32 = 10;
pub const DEFAULT_LEVEL_NAME: &str = "world";
pub const REACH: f64 = 6.0;
pub const EYE_HEIGHT: f64 = 1.62;
pub const PLAYER_HALF_WIDTH: f64 = 0.3;
pub const PLAYER_HEIGHT: f64 = 1.8;
pub const MANAGER_QUEUE: usize = 256;
pub const COORDINATOR_QUEUE: usize = 1024;
/// Ticks a [`World::tick_times`] subscriber may fall behind: a minute at
/// the default rate.
pub const TICK_TIMES_BACKLOG: usize = 1200;
const CHUNK_IO_FAILURE_WARNING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct WorldConfig {
    /// Where packet trace files are created.
    pub trace_dir: PathBuf,
    pub max_players: Option<u32>,
    pub online_mode: Option<bool>,
    /// Base URL of the profile API names are looked up against.
    pub profile_api_url: String,
    /// Base URL of the session server full profiles are fetched from.
    pub session_server_url: String,
    pub view_distance: Option<i32>,
    pub motd: Option<TextComponent>,
    /// The world directory; `<instance>/<level name>` when unset.
    pub directory: Option<PathBuf>,
    /// `level-name` from `server.properties` when unset.
    pub level_name: Option<String>,
    /// Seed for a new world; random if unset. Ignored for existing worlds.
    pub seed: Option<i64>,
    pub storage: Storage,
    pub backend_options: BackendOptions,
    /// Where `server.properties`, `ops.json` and the other lists live when
    /// neither `config_store` nor the world backend keeps them.
    pub instance_dir: PathBuf,
    /// Keeps the server configuration instead of the world backend or the
    /// instance directory.
    pub config_store: Option<Arc<dyn ConfigStore>>,
    /// Shows the icon from the config store in the server list.
    pub load_icon: bool,
    /// Overrides `white-list` from `server.properties` for this run.
    pub whitelist: Option<bool>,
    pub tick_lockstep: tick_barrier::TickLockstep,
    /// False binds no HTTP listener, now or on a later config change.
    pub serve: bool,
    /// `--config <key> <value>` pairs applied for this run on top of the
    /// saved settings; a bad key or value fails the open.
    pub config_overrides: Vec<(String, String)>,
    /// Reads the world and configuration but writes nothing to disk; a
    /// missing world lives in an in-memory server file for this run.
    pub no_save: bool,
    /// Everything the loaded features registered, already frozen
    /// (`crate::feature::load_all` ran before this was built). Empty for
    /// an inert core with no features.
    pub registries: Arc<crate::registry::Registries>,
}

/// Context shared by every actor.
pub struct Shared {
    /// Actors quarantined after a refused item operation.
    pub conservation: conservation::Conservation,
    pub stopwatches: commands::session_state::Stopwatches,
    pub random_sequences: commands::session_state::RandomSequences,
    /// The server-list state, set once the status actor exists; read for the
    /// Server Data packet a joining player gets.
    pub server_list: std::sync::OnceLock<watch::Receiver<Arc<crate::status::ServerListInfo>>>,
    /// server.properties values that change at runtime.
    pub live: live::LiveSettings,
    pub backend: Arc<dyn WorldBackend>,
    /// Where the world lives on disk (a directory or the backend's file),
    /// for features that keep their own files beside it; `None` when the
    /// world has no location (tests on memory backends).
    pub world_path: Option<std::path::PathBuf>,
    /// Chunk reads and writes, started on first use.
    pub chunk_io: chunk_io::ChunkIoCell,
    pub dimensions: Dimensions,
    pub online_mode: bool,
    /// The world spawn, written by the coordinator.
    pub spawn: watch::Receiver<Spawn>,
    pub difficulty: watch::Receiver<Difficulty>,
    pub game_rules: watch::Receiver<GameRules>,
    pub weather: watch::Receiver<Weather>,
    /// Who receives which world debug feeds, written by the coordinator.
    pub debug_subscribers: watch::Receiver<debug_feeds::SharedSubscribers>,
    pub seed: i64,
    /// Radius of the spawn area only operators may modify; 0 disables.
    pub spawn_protection: std::sync::atomic::AtomicI32,
    pub hardcore: std::sync::atomic::AtomicBool,
    pub difficulty_locked: std::sync::atomic::AtomicBool,
    pub ops_present: std::sync::atomic::AtomicBool,
    /// The external text filtering service, when configured.
    pub text_filter: Option<Arc<crate::text_filter::TextFilter>>,
    pub force_game_mode: std::sync::atomic::AtomicBool,
    /// Mojang's keys for checking players' chat keys; empty until fetched.
    pub profile_keys: Arc<crate::protocol::chat_session::ProfileKeyAuthority>,
    /// The game tick: advances only on ticks that run the simulation, so
    /// `/tick freeze` holds it. Its value is the world's game time.
    pub clock: watch::Receiver<u64>,
    /// Every server tick, frozen or not; starts equal to `clock`. Actors
    /// with work that must go on while frozen (players, chunk loading and
    /// sending, saving) tick on this and read `clock` to tell whether the
    /// game itself ticked.
    pub server_ticks: watch::Receiver<u64>,
    /// World clock state, written only by the coordinator.
    pub clocks: watch::Receiver<WorldClocks>,
    /// `/tick`'s target rate and freeze state, written only by the
    /// coordinator.
    pub tick_state: watch::Receiver<tick_rate::TickState>,
    /// What every ticking actor reports finished ticks to, so the
    /// coordinator can hold the clock until they catch up.
    pub tick_barrier: Arc<tick_barrier::TickBarrier>,
    /// Where every living player is, for regions to see players outside
    /// them; published by the coordinator.
    pub players: watch::Receiver<Arc<nearby::PlayerIndex>>,
    pub manager: mpsc::Sender<ManagerMessage>,
    pub coordinator: mpsc::Sender<CoordinatorMessage>,
    /// The generation pipeline of each dimension whose generator needs one.
    pub generation: Vec<Option<Arc<dyn generator::ChunkPipeline>>>,
    /// The commands players and the console can run.
    pub commands: Arc<commands::Registry>,
    /// Loaders for the entity types regions own, by saved type id.
    pub entity_types: Arc<entity::Registry>,
    /// The features' advancement tree and the triggers its criteria use;
    /// read [`Shared::advancements`], which has data packs' changes.
    pub base_advancements: Arc<dyn advancement::Advancements>,
    /// What blocks do on random and scheduled ticks and with bone meal.
    pub growth_behaviors: Arc<dyn growth::GrowthBehaviors>,
    /// What blocks drop when they break.
    pub block_loot: Arc<dyn drops::BlockLoot>,
    /// Makes primed TNT; `None` with no feature that has it.
    pub tnt: Option<Arc<dyn explosion::TntPrimer>>,
    /// Explosion block rules (see `explosion::ExplosionRules`).
    pub explosion_rules: Arc<dyn explosion::ExplosionRules>,
    /// Placement, shape and block tick rules (see `block_rules`).
    pub block_rules: Arc<dyn block_rules::BlockRules>,
    pub dispensers: Option<Arc<dyn region_api::Dispensers>>,
    pub block_entity_carrier: Option<Arc<dyn redstone::BlockEntityCarrier>>,
    pub block_entity_tickers: Arc<block_entity::BlockEntityTickers>,
    pub item_ports: Arc<tuff_item::registry::BlockTable<dyn item_port::ItemPort>>,
    /// Region behaviour features hook in, in registration order.
    pub region_hooks: Arc<[Arc<dyn region_api::RegionHooks>]>,
    /// Session behaviour features hook in, in registration order.
    pub session_hooks: Arc<[Arc<dyn session::hooks::SessionHooks>]>,
    pub player_saved: Arc<[Arc<Box<dyn session::hooks::PlayerSaved>>]>,
    pub login_gates: Arc<[Arc<dyn login_gate::LoginGates>]>,
    pub player_damage: Option<Arc<dyn session::hooks::PlayerDamage>>,
    pub fake_motion: Option<Arc<dyn fake::motion::FakeMotion>>,
    /// Packet interceptors, sorted by order.
    pub interceptors: Arc<[session::hooks::Interceptor]>,
    pub save_normalisers: crate::storage::normalise::SaveNormalisers,
    /// Every recipe, numbered for the recipe book.
    pub recipe_set: Arc<crafting::RecipeSet>,
    pub fluid_behaviors: Arc<dyn fluid_api::FluidBehaviors>,
    /// Builds each dimension's anti-xray plan.
    pub anti_xray: Arc<dyn anti_xray::AntiXrayPolicy>,
    /// What blocks do in the neighbour-changed/shape-update/signal
    /// cascade (`region/redstone.rs`), resolved from
    /// `WorldConfig::registries.block_behaviors` at open time; the inert
    /// table when no feature registered one.
    pub redstone_behaviors: Arc<dyn redstone::BlockBehaviors>,
    /// Anvil, grindstone and loom results.
    pub station_rules: Arc<dyn stations::StationRules>,
    /// The services features run beside the world (see `services`).
    pub services: services::Services,
    /// Per-player packet tracing (`/tuff tracing`).
    pub tracer: tuff_trace::Tracer,
    /// Seeds each region's random source; `None` draws one from the OS.
    pub random_seed: Option<u64>,
    /// Test hook: nonzero shuffles the entity tick's compute order by this seed.
    pub entity_compute_shuffle: std::sync::atomic::AtomicU64,
    /// Fewest entities per parallel compute task (`usize::MAX`: serial).
    pub entity_compute_min_batch: std::sync::atomic::AtomicUsize,
    /// Lock-free entity ID counter: IDs are unique across all regions.
    pub entity_ids: AtomicI32,
    /// The UUID of every entity living in any region of any dimension, so a
    /// saved entity whose UUID is already taken is rejected on load.
    pub entity_uuids: std::sync::Arc<entity_claim::EntityUuids>,
    /// Objectives, scores and teams.
    pub scoreboard: scoreboard::ScoreboardHandle,
    /// `/data ... storage` compounds.
    pub command_storage: command_storage::StorageHandle,
    /// Runtime settings: `tuff.*` and feature keys.
    pub config: crate::config::ConfigStore,
    /// The durable server records (lists, settings, feature grants).
    pub config_store: Arc<dyn ConfigStore>,
    /// The HTTP actor serving `tuff.http.*`; features may register further
    /// routes on it at runtime through `register_route`.
    pub http: tuff_http::HttpHandle,
    /// Base URL clients fetch served resources from.
    pub public_base_url: crate::public_ip::BaseUrl,
    /// Hosted resource packs and their delivery to players.
    pub resource_packs: crate::resource_pack::ResourcePacks,
    /// Validated custom items, blocks, entities and assets.
    pub custom: Arc<crate::custom::CustomContent>,
    /// Owns the generated pack built from the custom content assets.
    pub custom_pack: crate::custom_pack::CustomPack,
    /// The enabled data packs and what they load.
    pub datapacks: crate::datapack::DataPacks,
    /// The web admin panel's sessions, audit log and event bus.
    pub web: tuff_web_ui::Web,
    /// Live metrics: producers report here, the panel and features read.
    pub metrics: crate::metrics::Metrics,
    /// Permission nodes and every player's grants.
    pub permissions: crate::permission::Permissions,
    /// Which feature registered each hook, for timings.
    pub hook_owners: Arc<crate::feature::HookOwners>,
    /// Records and serves timings reports (`/tuff timings`).
    pub timings: tuff_timings::Timings,
    /// World backups (`/tuff backup`).
    pub backups: crate::storage::backup::jobs::Backups,
    /// Chunk pre-generation jobs (`/tuff chunk`).
    pub pregen: pregen::Pregen,
}

impl Shared {
    /// Asks the connection of the player with this uuid to change phase;
    /// true when it was in the phase the request applies to.
    pub async fn request_phase(
        &self,
        player: u128,
        request: impl FnOnce(oneshot::Sender<bool>) -> messages::PhaseRequest,
    ) -> bool {
        let (reply, answer) = oneshot::channel();
        let request = request(reply);
        if self
            .coordinator
            .send(CoordinatorMessage::Phase { player, request })
            .await
            .is_err()
        {
            return false;
        }
        answer.await.unwrap_or(false)
    }

    /// What saving the save gate allows now.
    pub fn saves(&self) -> chunk_io::SaveState {
        self.chunk_io.get(&self.backend).saves()
    }

    /// Records `uuid` as living; false if some entity already holds it.
    pub fn claim_entity_uuid(&self, uuid: u128) -> Option<entity_claim::EntityClaim> {
        self.entity_uuids.claim(uuid)
    }

    pub fn next_entity_id(&self) -> i32 {
        (self.entity_ids.fetch_add(1, Ordering::Relaxed) & i32::MAX).max(1)
    }

    /// The first of `count` consecutive entity ids, for an entity whose
    /// parts clients number after it (the ender dragon).
    pub fn next_entity_ids(&self, count: i32) -> i32 {
        loop {
            let first = self.entity_ids.fetch_add(count, Ordering::Relaxed) & i32::MAX;
            if first >= 1 && first <= i32::MAX - count {
                return first;
            }
        }
    }

    pub fn spawn(&self) -> Spawn {
        self.spawn.borrow().clone()
    }

    pub fn under_spawn_protection(
        &self,
        dimension: DimensionId,
        position: tuff_world_model::BlockPos,
        gamemaster: bool,
    ) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        let radius = self.spawn_protection.load(Relaxed);
        let spawn = self.spawn();
        radius > 0
            && self.ops_present.load(Relaxed)
            && !gamemaster
            && dimension == self.spawn_dimension()
            && (position.x - spawn.pos.x)
                .abs()
                .max((position.z - spawn.pos.z).abs())
                <= radius
    }

    pub fn spawn_dimension(&self) -> DimensionId {
        Self::resolve_spawn_dimension(&self.dimensions, &self.spawn.borrow())
    }

    pub fn difficulty(&self) -> Difficulty {
        *self.difficulty.borrow()
    }

    /// The features' loot rules, reading the loot tables data packs left in
    /// effect.
    pub fn block_loot(&self) -> Arc<dyn drops::BlockLoot> {
        self.datapacks
            .contents()
            .block_loot
            .clone()
            .unwrap_or_else(|| self.block_loot.clone())
    }

    /// The loot tables in effect: the built-in ones with whatever data
    /// packs replaced.
    pub fn loot_tables(&self) -> Arc<tuff_datapack::loot::LootTables> {
        self.datapacks.contents().loot_tables.clone()
    }

    /// The advancement tree: the features', with whatever data packs
    /// changed.
    pub fn advancements(&self) -> Arc<dyn advancement::Advancements> {
        self.advancements_in(&self.datapacks.contents())
    }

    /// The advancement tree `contents` has in effect.
    pub fn advancements_in(
        &self,
        contents: &crate::datapack::Contents,
    ) -> Arc<dyn advancement::Advancements> {
        contents
            .advancements
            .clone()
            .unwrap_or_else(|| self.base_advancements.clone())
    }

    /// The server's recipes: the features', with whatever data packs
    /// changed.
    pub fn recipes(&self) -> Arc<crafting::RecipeSet> {
        self.datapacks
            .contents()
            .recipes
            .clone()
            .unwrap_or_else(|| self.recipe_set.clone())
    }

    pub fn rule_bool(&self, name: &str) -> bool {
        self.game_rules.borrow().bool(name)
    }

    pub fn rule_int(&self, name: &str) -> i32 {
        self.game_rules.borrow().int(name)
    }

    /// Update Time with every clock, for a player entering the world.
    pub fn time_packet(&self) -> play::SetTime {
        let world_age = *self.clock.borrow() as i64;
        self.clocks.borrow().packet(world_age, None)
    }

    pub fn dimension(&self, id: DimensionId) -> &Dimension {
        self.dimensions.get(id)
    }

    pub fn generation(&self, id: DimensionId) -> Option<&Arc<dyn generator::ChunkPipeline>> {
        self.generation.get(id.index()).and_then(Option::as_ref)
    }

    /// The spawn's dimension, falling back to the default when the data
    /// names a dimension that no longer exists.
    pub fn resolve_spawn_dimension(dimensions: &Dimensions, spawn: &Spawn) -> DimensionId {
        dimensions
            .by_key(&spawn.dimension)
            .map_or_else(|| dimensions.default_id(), |d| d.id)
    }
}

/// A handle to a running world.
#[derive(Clone)]
pub struct World {
    coordinator: mpsc::Sender<CoordinatorMessage>,
    shared: Arc<Shared>,
    tick_times: tokio::sync::broadcast::Sender<tick_rate::TickTime>,
}

impl World {
    /// The world's shared state, for embedders that drive it directly
    /// (the headless game test runner).
    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    pub fn attach_server_list(&self, list: watch::Receiver<Arc<crate::status::ServerListInfo>>) {
        let _ = self.shared.server_list.set(list);
    }

    pub fn network(&self) -> Arc<NetworkSettings> {
        self.shared.live.network()
    }

    pub fn config(&self) -> &crate::config::ConfigStore {
        &self.shared.config
    }

    /// Opens (or creates) the world and starts its actors.
    pub async fn open(
        config: WorldConfig,
        status: mpsc::Sender<StatusCommand>,
        stop: mpsc::UnboundedSender<()>,
    ) -> io::Result<Self> {
        crate::logging::init();
        let level_name = match config.level_name.clone() {
            Some(name) => name,
            None => {
                let store = config
                    .config_store
                    .clone()
                    .unwrap_or_else(|| Arc::new(VanillaConfig::new(&config.instance_dir)));
                tokio::task::spawn_blocking(move || store.read_properties())
                    .await
                    .map_err(io::Error::other)??
                    .map_or_else(|| DEFAULT_LEVEL_NAME.to_owned(), |p| p.level_name())
            }
        };
        let directory = config
            .directory
            .clone()
            .unwrap_or_else(|| config.instance_dir.join(&level_name));
        let generators: Arc<dyn generator::WorldGenerators> = config
            .registries
            .generators
            .get()
            .cloned()
            .unwrap_or_else(|| Arc::new(generator::NoGenerators));
        let (backend, lock) = {
            let storage = config.storage.clone();
            let directory = directory.clone();
            let options = config.backend_options;
            let no_save = config.no_save;
            tokio::task::spawn_blocking(move || -> io::Result<_> {
                if no_save {
                    return crate::storage::open_no_save(&storage, &directory, options);
                }
                crate::storage::backup::apply_before_open(&storage, &directory, options)?;
                let backend = storage.open_with(&directory, options)?;
                let lock = backend.lock()?;
                backend.upgrade()?;
                let backend = crate::storage::journaled(backend, &directory)?;
                Ok((backend, lock))
            })
            .await
            .map_err(io::Error::other)??
        };
        let mut config_store = config
            .config_store
            .clone()
            .or_else(|| backend.config())
            .unwrap_or_else(|| Arc::new(VanillaConfig::new(&config.instance_dir)));
        if config.no_save {
            log::warn!("saving is disabled (--no-save): nothing is written to disk");
            config_store = Arc::new(tuff_storage::no_save::NoSaveConfig::new(config_store));
        }
        let (mut properties, lists, favicon, saved_settings, user_cache) = {
            let store = config_store.clone();
            let load_icon = config.load_icon;
            tokio::task::spawn_blocking(move || -> io::Result<_> {
                let (properties, lists, favicon) = load_config(&*store, load_icon)?;
                let user_cache = store.read_user_cache().unwrap_or_else(|err| {
                    log::warn!("ignoring the user cache: {err}");
                    Vec::new()
                });
                Ok((
                    properties,
                    lists,
                    favicon,
                    store.read_settings()?,
                    user_cache,
                ))
            })
            .await
            .map_err(io::Error::other)??
        };
        let (property_overrides, setting_overrides) =
            crate::property_spec::split_overrides(&config.config_overrides).map_err(|err| {
                io::Error::new(io::ErrorKind::InvalidInput, format!("--config: {err}"))
            })?;
        let mut property_baseline = std::collections::BTreeMap::new();
        if !property_overrides.is_empty() {
            let mut entries = properties.entries();
            for (key, _) in &property_overrides {
                property_baseline.insert(key.clone(), entries.get(key).cloned());
            }
            entries.extend(property_overrides);
            properties = ServerProperties::from_entries(entries);
        }
        let settings = crate::config::ConfigStore::new();
        for key in &config.registries.config_keys {
            settings
                .register(key.clone())
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
        }
        for (name, err) in
            settings.load_saved(saved_settings.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        {
            if !matches!(err, crate::config::ConfigError::Unknown(_)) {
                log::warn!("ignoring saved setting {name}: {err}");
            }
        }
        for (name, text) in &setting_overrides {
            settings.set_text_for_run(name, text).map_err(|err| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid config override {name}: {err}"),
                )
            })?;
        }
        backend.set_entity_index(settings.sqlite_entity_index());
        let code_of_conduct = if properties.enable_code_of_conduct() {
            let instance_dir = config.instance_dir.clone();
            match tokio::task::spawn_blocking(move || crate::code_of_conduct::read(&instance_dir))
                .await
                .map_err(io::Error::other)?
            {
                Ok(texts) => texts.into_iter().collect(),
                Err(err) => return Err(err),
            }
        } else {
            Vec::new()
        };
        let seed = config
            .seed
            .or_else(|| properties.level_seed_value())
            .unwrap_or_else(rand::random);
        let overworld_enabled = dimension::enabled(&settings, dimension::OVERWORLD);
        let spawn_unset = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut level = {
            let backend = backend.clone();
            let spawn_unset = spawn_unset.clone();
            tokio::task::spawn_blocking(move || {
                LevelData::load(
                    backend.documents(),
                    || seed,
                    |_| {
                        spawn_unset.store(true, std::sync::atomic::Ordering::Relaxed);
                        tuff_world_model::BlockPos::new(0, 64, 0)
                    },
                )
            })
            .await
            .map_err(io::Error::other)??
        };
        let spawn_unset = spawn_unset.load(std::sync::atomic::Ordering::Relaxed);
        level.set_new_world_gameplay(
            properties.difficulty(),
            properties.gamemode(),
            properties.hardcore(),
            properties.pvp(),
        );
        level.sync_existing_world_gameplay(properties.hardcore(), properties.pvp());
        level.sync_existing_world_defaults(properties.difficulty(), properties.gamemode());
        if !level.new_world {
            let facts = crate::property_spec::WorldFacts {
                seed: level.seed,
                level_name: &level_name,
            };
            for note in crate::property_spec::startup_creation_notes(&properties.entries(), &facts)
            {
                log::warn!("{note}");
            }
        }
        level.set_new_world_structures(properties.generate_structures());
        if let Some(whitelist) = config.whitelist {
            properties.white_list = whitelist;
        }
        let max_players = config
            .max_players
            .unwrap_or_else(|| properties.max_players());
        let view_distance = config
            .view_distance
            .unwrap_or_else(|| properties.view_distance());
        let _ = status.send(StatusCommand::SetMaxPlayers(max_players)).await;
        let _ = status
            .send(StatusCommand::SetPolicy {
                hide_players: properties.hide_online_players(),
                enforces_secure_chat: properties.enforce_secure_profile(),
            })
            .await;
        let simulation = SimulationSettings {
            simulation_distance: properties.simulation_distance(),
            pause_when_empty_seconds: properties.pause_when_empty_seconds(),
            broadcast_range_percent: properties.entity_broadcast_range_percentage(),
        };
        let mut network = NetworkSettings {
            online_mode: config
                .online_mode
                .unwrap_or_else(|| properties.online_mode()),
            port: properties.server_port(),
            ip: properties.server_ip(),
            instance_dir: Some(config.instance_dir.clone()),
            query_port: properties.enable_query().then(|| properties.query_port()),
            level_name: properties.level_name(),
            ..NetworkSettings::default()
        };
        network.apply(&properties);
        network.code_of_conduct = code_of_conduct;
        if config.motd.is_none() {
            let _ = status
                .send(StatusCommand::SetMotd(TextComponent::new(
                    properties.motd(),
                )))
                .await;
        }
        if let Some(favicon) = favicon {
            let _ = status.send(StatusCommand::SetFavicon(Some(favicon))).await;
        }
        let (clock_tx, clock) = watch::channel(level.game_time.max(0) as u64);
        let (server_ticks_tx, server_ticks) = watch::channel(*clock.borrow());
        let (clocks_tx, clocks) = watch::channel(level.clocks.clone());
        let (tick_state_tx, tick_state) = watch::channel(tick_rate::TickState::default());
        let (difficulty_tx, difficulty) = watch::channel(level.difficulty);
        let (rules_tx, game_rules) = watch::channel(level.game_rules.clone());
        let (weather_tx, weather) = watch::channel(level.weather);
        let (debug_subscribers_tx, debug_subscribers) = watch::channel(Arc::default());
        let (players_tx, players) = watch::channel(Arc::default());
        let (manager, manager_rx) = mpsc::channel(MANAGER_QUEUE);
        let (coordinator, coordinator_rx) = mpsc::channel(COORDINATOR_QUEUE);
        let provider_context = custom_dimension::ProviderContext {
            config_store: config_store.clone(),
            settings: settings.clone(),
            properties: properties.clone(),
        };
        let resolved = Resolved::from_registries(&config.registries)
            .with_provided(&config.registries, &provider_context);
        let (packs, features) = if level.new_world {
            (properties.initial_data_packs(), level.enabled_features())
        } else {
            let saved = (level.data_packs(), level.enabled_features());
            match properties.startup_feature_packs() {
                Some(wanted) => {
                    crate::datapack::apply_startup_feature_packs(saved.0, saved.1, &wanted)
                }
                None => saved,
            }
        };
        let datapacks = crate::datapack::DataPacks::open(
            backend.clone(),
            resolved.commands.clone(),
            crate::datapack::Base {
                function_permission: properties.function_permission_level,
                ..crate::datapack::Base::new(&resolved)
            },
            packs,
            features,
        )
        .await;
        level.set_data_packs(&datapacks.config());
        level.set_enabled_features(&datapacks.features());
        rules_tx.send_modify(|rules| rules.apply_features(&datapacks.features()));
        let contents = datapacks.contents();
        world_preset::apply(
            &mut level,
            &contents.registries,
            &properties.level_type(),
            &properties.generator_settings(),
        );
        let pack_types = custom_dimension::PackTypes::from_registries(&contents.registries);
        let custom = {
            for provider in &config.registries.dimension_providers {
                provider.attach(&provider_context);
            }
            let taken: Vec<&str> = level.dimensions.iter().map(|d| d.key.as_str()).collect();
            custom_dimension::CustomDimensions::collect_in(
                &config.registries.dimension_providers,
                &taken,
                &contents.registries,
                &pack_types,
            )
        };
        let stems = world_preset::stems(&contents.registries);
        let all_dimensions = world_preset::with_stems(&level, &stems, backend.documents());
        let enabled = dimension::enabled_settings(&all_dimensions, &settings);
        let mut dimensions =
            Dimensions::from_settings_typed(&enabled, level.seed, &*generators, &pack_types)
                .with_custom(custom, level.seed);
        let custom_keys: Vec<String> = dimensions
            .custom()
            .dimensions()
            .iter()
            .map(|resolved| resolved.key.clone())
            .collect();
        for key in custom_keys {
            match crate::storage::level::WorldBorder::read(backend.documents(), &key) {
                Ok(saved) => dimensions.restore_border(&key, &saved),
                Err(err) => log::warn!("cannot read the border of {key}: {err}"),
            }
        }
        dimensions.reserve(all_dimensions.iter().map(|d| d.key.as_str()));
        if dimensions.iter().len() == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, NO_DIMENSIONS));
        }
        dimensions.apply_data_packs(&contents.registries, &contents.tags, &settings);
        if spawn_unset && let Some(pos) = world_preset::spawn(&dimensions, &contents.tags).await? {
            level.spawn.pos = pos;
        }
        let chosen = dimensions.choose_spawn(
            &settings
                .get_string(dimension::SPAWN_DIMENSION_KEY)
                .unwrap_or_default(),
        );
        dimensions.place_spawn(chosen, &mut level.spawn);
        let bonus_chest_at = (level.bonus_chest_pending && overworld_enabled)
            .then_some(level.spawn.pos)
            .filter(|_| level.spawn.dimension == crate::storage::level::OVERWORLD);
        let (spawn_tx, spawn) = watch::channel(level.spawn.clone());
        dimensions.limit_world_size(network.max_world_size);
        let borders = dimensions.take_border_writers();
        let metrics = crate::metrics::Metrics::spawn(Default::default());
        let chunk_io = chunk_io::ChunkIo::start_supervised(
            backend.clone(),
            metrics.clone(),
            tuff_supervise::Fatal::channel(stop.clone()),
        );
        let backend = chunk_io.backend().clone();
        let mut chunk_io_failures = chunk_io.failures();
        let services = services::Services::create(&config.registries.services, &dimensions);
        let (scoreboard, _) = scoreboard::start(backend.clone(), clock.clone());
        let (command_storage, _) = command_storage::start(backend.clone());
        let generation = generator::start_pipelines(
            &dimensions,
            &server_ticks,
            Some((&chunk_io, &backend.chunk_codec())),
            &tuff_datapack::tags::TagFeed::new(datapacks.watch_contents()),
        );
        let http_handle = crate::http::spawn_metered(&metrics);
        let public_base_url = crate::public_ip::start();
        let resource_packs =
            crate::resource_pack::ResourcePacks::spawn(public_base_url.clone(), settings.clone());
        let custom_pack = crate::custom_pack::CustomPack::start(
            resource_packs.clone(),
            settings.clone(),
            resolved.custom.asset_sets().to_vec(),
        )
        .await;
        let web = tuff_web_ui::Web::spawn(Some(config_store.clone()), Default::default()).await;
        let permissions = crate::permission::Permissions::open(
            Some(config_store.clone()),
            config.registries.permissions.nodes(),
        )
        .await;
        let dimension_names = dimensions.iter().map(|d| d.key.clone()).collect();
        let online_mode = network.online_mode || crate::net::velocity::enabled(&settings);
        let live_settings = live::LiveSettings::new(
            view_distance.max(2),
            max_players,
            level.default_game_mode,
            simulation,
            network,
        );
        live_settings.set_max_chained_neighbor_updates(properties.max_chained_neighbor_updates());
        let shared = Arc::new(Shared {
            conservation: Default::default(),
            stopwatches: commands::session_state::Stopwatches::load(backend.clone()),
            random_sequences: commands::session_state::RandomSequences::load(backend.clone()),
            server_list: std::sync::OnceLock::new(),
            online_mode,
            live: live_settings,
            chunk_io: chunk_io::ChunkIoCell::with(chunk_io),
            backend: backend.clone(),
            world_path: Some(directory.clone()),
            dimensions,
            spawn,
            difficulty,
            game_rules,
            weather,
            debug_subscribers,
            seed: level.seed,
            spawn_protection: Default::default(),
            hardcore: std::sync::atomic::AtomicBool::new(level.hardcore),
            difficulty_locked: std::sync::atomic::AtomicBool::new(level.difficulty_locked),
            ops_present: Default::default(),
            text_filter: {
                let (config, version) = properties.text_filtering();
                match crate::text_filter::TextFilter::from_properties(&config, version) {
                    Ok(filter) => filter.map(Arc::new),
                    Err(reason) => {
                        log::warn!("Could not create text filter: {reason}");
                        None
                    }
                }
            },
            force_game_mode: Default::default(),
            profile_keys: {
                let authority = crate::protocol::chat_session::ProfileKeyAuthority::default();
                authority.set_enforcing(
                    config
                        .online_mode
                        .unwrap_or_else(|| properties.online_mode())
                        && properties.enforce_secure_profile(),
                );
                Arc::new(authority)
            },
            clock,
            server_ticks,
            clocks,
            tick_state,
            tick_barrier: tick_barrier::TickBarrier::new(config.tick_lockstep),
            players,
            manager,
            coordinator: coordinator.clone(),
            generation,
            commands: resolved.commands,
            entity_types: resolved.entity_types,
            base_advancements: resolved.advancements,
            growth_behaviors: resolved.growth_behaviors,
            block_loot: resolved.block_loot,
            tnt: resolved.tnt,
            explosion_rules: resolved.explosion_rules,
            block_rules: resolved.block_rules,
            dispensers: resolved.dispensers,
            block_entity_carrier: resolved.block_entity_carrier,
            block_entity_tickers: resolved.block_entity_tickers,
            item_ports: resolved.item_ports,
            region_hooks: resolved.region_hooks,
            session_hooks: resolved.session_hooks,
            player_saved: resolved.player_saved,
            login_gates: resolved.login_gates,
            player_damage: resolved.player_damage,
            fake_motion: resolved.fake_motion,
            interceptors: resolved.interceptors,
            save_normalisers: resolved.save_normalisers,
            recipe_set: resolved.recipe_set,
            fluid_behaviors: resolved.fluid_behaviors,
            anti_xray: resolved.anti_xray,
            redstone_behaviors: resolved.redstone_behaviors,
            station_rules: resolved.station_rules,
            services,
            tracer: crate::trace::spawn_tracer(config.trace_dir.clone()),
            random_seed: None,
            entity_compute_shuffle: Default::default(),
            entity_compute_min_batch: crate::world::entity::compute_min_batch().into(),
            entity_ids: AtomicI32::new(1),
            entity_uuids: Default::default(),
            scoreboard,
            command_storage,
            config: settings.clone(),
            config_store: config_store.clone(),
            http: http_handle.clone(),
            public_base_url,
            resource_packs: resource_packs.clone(),
            custom: resolved.custom.clone(),
            custom_pack,
            datapacks,
            web,
            metrics,
            permissions,
            timings: tuff_timings::Timings::spawn_with(
                Some(Arc::new(crate::storage::ReportStore(config_store.clone()))),
                resolved.hook_owners.features.clone(),
                tuff_timings::Labels {
                    dimensions: dimension_names,
                    server_name: coordinator::SERVER_BRAND.to_owned(),
                },
                |runner| {
                    tokio::spawn(tuff_supervise::restarting(
                        "the timings recorder",
                        runner,
                        |runner| Box::pin(runner.run()),
                    ));
                },
            ),
            hook_owners: resolved.hook_owners,
            backups: crate::storage::backup::jobs::Backups::spawn(
                crate::storage::backup::jobs::BackupsConfig {
                    instance_dir: Some(config.instance_dir.clone()),
                    level: level_name.clone(),
                    settings: settings.clone(),
                },
            ),
            pregen: pregen::Pregen::spawn(),
        });
        tuff_item::item::conservation::set_checks(
            shared
                .config
                .get_bool(crate::config::ITEMS_CONSERVATION_CHECKS)
                .unwrap_or(true),
        );
        shared.backups.attach(&shared);
        shared.services.start(&shared);
        {
            let bind_config = crate::http::config_from_store(&shared.config);
            if config.serve
                && let Err(err) = shared
                    .http
                    .bind(tuff_http::MAIN_LISTENER, bind_config)
                    .await
            {
                log::error!("failed to bind the http server: {err:#}");
            }
            reconfigure_base_url(&shared, &config.registries.public_ip_endpoints).await;
            shared
                .http
                .register_route(crate::resource_pack::ROUTE_PREFIX, resource_packs.route())
                .await;
            for pack in config.registries.resource_packs.iter() {
                resource_packs.register(pack.clone()).await;
            }
            crate::panel_host::resource_packs::restore(&shared).await;
            for (prefix, handler) in config.registries.http_routes.iter() {
                shared.http.register_route(prefix, handler.clone()).await;
            }
            crate::panel_host::start(&shared, &config.registries.panel).await;
            tuff_web_ui::announce(&*shared).await;
        }
        tuff_supervise::spawn(
            "the http rebinder",
            rebind_http_on_config_change(
                shared.clone(),
                config.serve,
                config.registries.public_ip_endpoints.clone(),
            ),
        );
        {
            let level = level.clone();
            let name = level_name.clone();
            tokio::task::spawn_blocking(move || {
                level.save(backend.documents(), &name, coordinator::SERVER_BRAND)
            })
            .await
            .map_err(io::Error::other)??;
        }
        let profile_lookup = crate::auth::ProfileLookup::new(&config.profile_api_url)
            .map_err(io::Error::other)?
            .with_session_server(&config.session_server_url);
        let (config_handle, config_task) = config_actor::start(config_store, coordinator.clone());
        {
            let weak = config_handle.downgrade();
            settings.set_sink(Box::new(move |pairs| {
                if let Some(handle) = weak.upgrade() {
                    let _ = handle.send(config_actor::ConfigRequest::Settings(pairs));
                }
            }));
        }
        tokio::spawn(RegionManager::new(shared.clone(), manager_rx).run());
        let (tick_times, _) = tokio::sync::broadcast::channel(TICK_TIMES_BACKLOG);
        tokio::spawn(
            Coordinator::new(coordinator::CoordinatorConfig {
                shared: shared.clone(),
                messages: coordinator_rx,
                clock: clock_tx,
                server_ticks: server_ticks_tx,
                clocks: clocks_tx,
                tick_state: tick_state_tx,
                spawn: spawn_tx,
                difficulty: difficulty_tx,
                game_rules: rules_tx,
                weather: weather_tx,
                debug_subscribers: debug_subscribers_tx,
                borders,
                tick_times: tick_times.clone(),
                players: players_tx,
                level,
                level_name,
                status,
                lock,
                properties,
                property_baseline,
                lists,
                user_cache,
                config: config_handle,
                config_task,
                stop,
                profile_lookup,
            })
            .run(),
        );
        {
            let coordinator = coordinator.downgrade();
            tokio::spawn(async move {
                while chunk_io_failures.changed().await.is_ok() {
                    let Some(coordinator) = coordinator.upgrade() else {
                        break;
                    };
                    if coordinator
                        .send(CoordinatorMessage::ChunkIoFailed)
                        .await
                        .is_err()
                    {
                        break;
                    }
                    tokio::time::sleep(CHUNK_IO_FAILURE_WARNING_INTERVAL).await;
                }
            });
        }
        if let Some(pos) = bonus_chest_at {
            let shared = shared.clone();
            tokio::spawn(async move {
                let line = format!(
                    "place feature minecraft:bonus_chest {} {} {}",
                    pos.x, pos.y, pos.z
                );
                commands::console::run(shared, &line).await;
            });
        }
        Ok(Self {
            coordinator,
            shared,
            tick_times,
        })
    }

    /// Every finished tick's duration from now on. A subscriber that falls
    /// more than [`TICK_TIMES_BACKLOG`] ticks behind loses the oldest.
    pub fn tick_times(&self) -> tokio::sync::broadcast::Receiver<tick_rate::TickTime> {
        self.tick_times.subscribe()
    }

    /// Whether `user` connecting from `address` may join, and the level
    /// they get; the outer error means the world has stopped.
    pub async fn can_login(
        &self,
        user: tuff_storage::admin::NameAndId,
        address: std::net::IpAddr,
    ) -> io::Result<Result<tuff_storage::admin::PermissionLevel, TextComponent>> {
        let (reply, answer) = oneshot::channel();
        let stopped = || io::Error::other("world is not running");
        self.coordinator
            .send(CoordinatorMessage::CanLogin {
                user,
                address,
                reply,
            })
            .await
            .map_err(|_| stopped())?;
        answer.await.map_err(|_| stopped())
    }

    /// Addresses the HTTP server's main listener is bound to; empty if it
    /// failed to bind at startup.
    pub async fn http_addrs(&self) -> Vec<std::net::SocketAddr> {
        self.shared.http.addrs(tuff_http::MAIN_LISTENER).await
    }

    pub fn web(&self) -> tuff_web_ui::Web {
        self.shared.web.clone()
    }

    pub fn metrics(&self) -> crate::metrics::Metrics {
        self.shared.metrics.clone()
    }

    /// Holds saving still for a job copying the world's files (see
    /// [`save_gate::hold_saves`]).
    pub async fn hold_saves(
        &self,
        owner: &str,
        limit: std::time::Duration,
    ) -> Result<save_gate::SaveHold, save_gate::HoldError> {
        save_gate::hold_saves(&self.shared, owner, limit).await
    }

    pub fn profile_keys(&self) -> Arc<crate::protocol::chat_session::ProfileKeyAuthority> {
        self.shared.profile_keys.clone()
    }

    pub fn tracer(&self) -> tuff_trace::Tracer {
        self.shared.tracer.clone()
    }

    pub fn timings(&self) -> tuff_timings::Timings {
        self.shared.timings.clone()
    }

    /// The plugin dimensions, whose types and biomes clients learn while
    /// configuring.
    pub fn custom_dimensions(&self) -> &custom_dimension::CustomDimensions {
        self.shared.dimensions.custom()
    }

    /// The world's feature flags, which clients learn while configuring.
    pub fn features(&self) -> Arc<[String]> {
        self.shared.datapacks.features()
    }

    /// The Update Tags body for the loaded data packs.
    pub fn registries(&self) -> Arc<tuff_item::registries::Registries> {
        self.shared.datapacks.contents().registries.clone()
    }

    pub fn tags_packet(&self) -> bytes::Bytes {
        self.shared.datapacks.contents().tags_packet.clone()
    }

    pub fn resource_packs(&self) -> crate::resource_pack::ResourcePacks {
        self.shared.resource_packs.clone()
    }

    pub fn custom_pack(&self) -> crate::custom_pack::CustomPack {
        self.shared.custom_pack.clone()
    }

    pub fn public_base_url(&self) -> crate::public_ip::BaseUrl {
        self.shared.public_base_url.clone()
    }

    /// Runs `line` as the console, returning the feedback it produced.
    pub async fn run_console_command(&self, line: &str) -> Vec<TextComponent> {
        commands::console::run(self.shared.clone(), line).await
    }

    /// Completions for `line` as the owner console; offsets are relative
    /// to `line` as typed.
    pub async fn complete_console_command(&self, line: &str) -> crate::command::Suggestions {
        let mut found =
            commands::console::suggest(&self.shared, line, crate::permission::Subject::Server)
                .await;
        if !line.starts_with('/') {
            found.start = found.start.saturating_sub(1);
        }
        found
    }

    /// Runs `line` as `name` with `level`, returning its feedback.
    pub async fn run_command_as(
        &self,
        line: &str,
        name: String,
        level: tuff_storage::admin::PermissionLevel,
    ) -> Vec<TextComponent> {
        commands::console::run_as(self.shared.clone(), line, name, level).await
    }

    /// Admits a player. The outer error means the world has stopped; the
    /// inner one is a reason to show the player.
    pub async fn join(
        &self,
        profile: GameProfile,
        info: ClientInformation,
        brand: Option<String>,
        address: std::net::IpAddr,
        outbound: mpsc::UnboundedSender<Bytes>,
        phase: mpsc::UnboundedSender<messages::PhaseRequest>,
    ) -> io::Result<Result<Joined, TextComponent>> {
        let (reply, joined) = oneshot::channel();
        let stopped = || io::Error::other("world is not running");
        self.coordinator
            .send(CoordinatorMessage::Join {
                profile,
                info,
                brand,
                address,
                outbound,
                phase,
                reply,
            })
            .await
            .map_err(|_| stopped())?;
        joined.await.map_err(|_| stopped())
    }

    /// Admits a fake player owned by `owner`; see [`fake`].
    pub async fn spawn_fake(
        &self,
        owner: &str,
        spec: fake::FakeSpec,
    ) -> io::Result<Result<fake::FakeHandle, String>> {
        let (reply, handle) = oneshot::channel();
        let stopped = || io::Error::other("world is not running");
        self.coordinator
            .send(CoordinatorMessage::SpawnFake {
                owner: owner.to_owned(),
                spec,
                reply,
            })
            .await
            .map_err(|_| stopped())?;
        handle.await.map_err(|_| stopped())
    }

    /// Removes one fake of `owner` (by uuid) or all of them; returns how
    /// many were asked to leave.
    pub async fn remove_fakes(&self, owner: &str, uuid: Option<u128>) -> io::Result<usize> {
        let (reply, count) = oneshot::channel();
        let stopped = || io::Error::other("world is not running");
        self.coordinator
            .send(CoordinatorMessage::RemoveFake {
                owner: owner.to_owned(),
                uuid,
                reply,
            })
            .await
            .map_err(|_| stopped())?;
        count.await.map_err(|_| stopped())
    }

    /// Disconnects everyone, saves everything, and stops the world.
    pub async fn shutdown(&self) {
        let (done, finished) = oneshot::channel();
        if self
            .coordinator
            .send(CoordinatorMessage::Shutdown { done })
            .await
            .is_ok()
        {
            let _ = finished.await;
        }
    }
}

mod lookups;
pub use lookups::*;
mod reconfigure;
use reconfigure::*;
mod config;
pub use config::*;
mod settings;
pub use settings::*;
