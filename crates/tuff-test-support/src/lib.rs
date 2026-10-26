//! Tests of the region actors and their manager, driven through the same
//! channels player sessions use.

#![expect(
    clippy::disallowed_methods,
    reason = "test support crate holds test fixtures only"
)]

pub use std::collections::HashSet;
pub use std::path::Path;
pub use std::sync::Arc;
pub use std::sync::atomic::AtomicI32;
pub use std::time::Duration;
use tuff_core::storage::codec::WorldBackendExt as _;

pub use rand::SeedableRng;
pub use rand::rngs::StdRng;
pub use tokio::sync::{mpsc, oneshot, watch};
pub use tuff_vanilla_data::{Block, Item};

pub mod admin_client;
mod clicks;
pub mod placeholder;
mod saves;
pub mod scenario;
pub use clicks::{offering, place};
pub use scenario::*;

pub use placeholder::Overworld;
pub use tuff_core::protocol::codec::ReadExt as _;
pub use tuff_core::protocol::play;
pub use tuff_core::storage::Storage;
pub use tuff_core::storage::chunk_nbt;
pub use tuff_core::storage::level::Spawn;
pub use tuff_core::storage::level::{DimensionSettings, OVERWORLD, THE_NETHER};
pub use tuff_core::world::dimension::{DimensionId, Dimensions};
pub use tuff_core::world::generator::ChunkGenerator;
pub use tuff_core::world::manager::RegionManager;
pub use tuff_core::world::menu::MenuKind;
pub use tuff_core::world::messages::CoordinatorMessage;
pub use tuff_core::world::messages::{
    ActionOutcome, BlockUpdate, BreakMode, ContainerChange, ManagerMessage, MenuOpen, MenuSource,
    Place, PlayerEvent, PlayerHandle, PlayerUpdate, RegionHandle, RegionInternal, RegionRequest,
    Teleport,
};
pub use tuff_core::world::player::Difficulty;
pub use tuff_core::world::*;
pub use tuff_item::item::{BurnReason, ItemSnapshot, ItemStack, MintReason};
pub use tuff_item::text::TextComponent;
pub use tuff_nbt::{Compound, NamedTag, NbtLimits, Tag};
pub use tuff_storage::backend::WorldBackend;
pub use tuff_storage::region_file::RegionFile;
pub use tuff_storage::test_dir::TestDir;
pub use tuff_storage::vanilla::VanillaBackend;
pub use tuff_world_model::BlockPos;
pub use tuff_world_model::chunk::ChunkShape;
pub use tuff_world_model::chunk_meta::Decoded;
pub use tuff_world_model::light::{SectionLight, min_light_section};
pub use tuff_world_model::pos::{ChunkPos, RegionPos};

pub type TransactionAnswer = Option<(bool, Vec<(BlockPos, Vec<Option<ItemSnapshot>>)>)>;

/// The overworld's placeholder terrain with the test seed.
pub struct Generator(Overworld);

impl Generator {
    pub fn new(seed: i64) -> Self {
        Self(Overworld::new(seed))
    }

    pub fn height(&self, x: i32, z: i32) -> i32 {
        self.0.height(x, z)
    }

    pub fn generate(&self, pos: ChunkPos) -> tuff_world_model::chunk::Chunk {
        self.0.generate(pos, ChunkShape::OVERWORLD)
    }

    pub fn spawn(&self) -> BlockPos {
        self.0.spawn()
    }
}

pub const TIMEOUT: Duration = Duration::from_secs(20);

/// How long one tick may take to settle (chunk generation included).
pub const TICK_TIMEOUT: Duration = Duration::from_secs(120);

/// The frozen registries of the vanilla feature set, loaded through the
/// same `feature::load_all` path a real server uses, so tests exercise
/// vanilla behavior the way it is actually wired. Mirrors the tuff
/// binary's `vanilla_features::all()` list, which this crate cannot depend on
/// (it lives in a binary crate); keep the two in step.
pub fn vanilla_registries() -> Arc<tuff_core::registry::Registries> {
    static REGISTRIES: std::sync::OnceLock<Arc<tuff_core::registry::Registries>> =
        std::sync::OnceLock::new();
    REGISTRIES
        .get_or_init(|| {
            let mut registries = tuff_core::registry::Registries::new();
            tuff_core::feature::load_all(&vanilla_features(), &mut registries)
                .expect("vanilla features load");
            Arc::new(registries)
        })
        .clone()
}

pub fn vanilla_features() -> Vec<Box<dyn tuff_core::feature::Feature>> {
    vec![
        Box::new(tuff_feature_builtin_vanilla_worldgen::WorldgenFeature),
        Box::new(tuff_feature_builtin_vanilla_redstone::RedstoneFeature),
        Box::new(tuff_feature_builtin_vanilla_items::ItemsFeature),
        Box::new(tuff_feature_builtin_vanilla_maps::MapsFeature),
        Box::new(tuff_feature_builtin_vanilla_recipes::RecipesFeature),
        Box::new(tuff_feature_builtin_vanilla_loot::LootFeature),
        Box::new(tuff_feature_builtin_vanilla_blocks::BlocksFeature),
        Box::new(tuff_feature_builtin_vanilla_combat::CombatFeature),
        Box::new(tuff_feature_builtin_vanilla_players::PlayersFeature),
        Box::new(tuff_feature_builtin_vanilla_entities::EntitiesFeature),
        Box::new(tuff_feature_builtin_vanilla_mobs::MobsFeature),
        Box::new(tuff_feature_builtin_vanilla_survival::SurvivalFeature),
        Box::new(tuff_feature_builtin_vanilla_dimensions::DimensionsFeature),
        Box::new(tuff_feature_builtin_core_commands::CoreCommandsFeature),
        Box::new(tuff_feature_builtin_vanilla_commands::VanillaCommandsFeature),
        Box::new(tuff_feature_builtin_vanilla_anticheat::AnticheatFeature),
    ]
}

static NEXT_DETACHED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn detached_config_store() -> Arc<dyn tuff_storage::config::ConfigStore> {
    Arc::new(tuff_storage::vanilla::config::VanillaConfig::new(
        std::env::temp_dir().join(format!(
            "tuff-detached-config-{}-{}",
            std::process::id(),
            NEXT_DETACHED.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )),
    ))
}

/// The dimensions `registries`' providers add to a test world.
pub fn plugin_dimensions(
    registries: &tuff_core::registry::Registries,
) -> tuff_core::world::custom_dimension::CustomDimensions {
    let vanilla = DimensionSettings::vanilla();
    let taken: Vec<&str> = vanilla.iter().map(|d| d.key.as_str()).collect();
    tuff_core::world::custom_dimension::CustomDimensions::collect(
        &registries.dimension_providers,
        &taken,
    )
}

/// World-wide state with no actors behind it, for unit tests of code that
/// only reads it (game rules, difficulty, dimensions).
pub fn bare_shared() -> Shared {
    tuff_core::logging::init();
    let dimensions =
        Dimensions::from_settings_with(&DimensionSettings::vanilla(), 1, placeholder::for_settings);
    let spawn = Spawn {
        dimension: OVERWORLD.into(),
        pos: BlockPos::new(0, 80, 0),
        yaw: 0.0,
        pitch: 0.0,
    };
    let registries = vanilla_registries();
    let resolved = Resolved::from_registries(&registries);
    Shared {
        conservation: Default::default(),
        stopwatches: Default::default(),
        random_sequences: Default::default(),
        server_list: std::sync::OnceLock::new(),
        live: tuff_core::world::live::LiveSettings::new(
            2,
            10,
            play::GameMode::Survival,
            Default::default(),
            Default::default(),
        ),
        chunk_io: Default::default(),
        backend: Arc::new(tuff_storage::memory::MemoryBackend::default()),
        world_path: None,
        dimensions,
        online_mode: false,
        spawn: watch::channel(spawn).1,
        difficulty: watch::channel(Difficulty::Normal).1,
        game_rules: watch::channel(tuff_core::world::game_rules::GameRules::default()).1,
        weather: watch::channel(tuff_core::world::weather::Weather::default()).1,
        debug_subscribers: watch::channel(Default::default()).1,
        seed: 1,
        spawn_protection: Default::default(),
        hardcore: Default::default(),
        difficulty_locked: Default::default(),
        ops_present: Default::default(),
        text_filter: None,
        force_game_mode: Default::default(),
        profile_keys: Default::default(),
        clock: watch::channel(1).1,
        server_ticks: watch::channel(1).1,
        clocks: watch::channel(tuff_core::world::clock::WorldClocks::default()).1,
        tick_state: watch::channel(tuff_core::world::tick_rate::TickState::default()).1,
        players: watch::channel(Arc::default()).1,
        manager: mpsc::channel(1).0,
        coordinator: mpsc::channel(1).0,
        generation: Vec::new(),
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
        services: Default::default(),
        tracer: tuff_trace::Tracer::inert(),
        random_seed: Some(1),
        entity_compute_shuffle: Default::default(),
        entity_compute_min_batch: Default::default(),
        entity_ids: AtomicI32::new(1),
        entity_uuids: Default::default(),
        tick_barrier: tuff_core::world::tick_barrier::TickBarrier::new(Default::default()),
        scoreboard: tuff_core::world::scoreboard::ScoreboardHandle::detached(),
        command_storage: tuff_core::world::command_storage::StorageHandle::detached(),
        config: tuff_core::config::ConfigStore::new(),
        http: tuff_http::HttpHandle::detached(),
        public_base_url: tuff_core::public_ip::BaseUrl::detached("http://127.0.0.1:0"),
        resource_packs: tuff_core::resource_pack::ResourcePacks::detached(),
        custom: Default::default(),
        custom_pack: Default::default(),
        datapacks: tuff_core::datapack::DataPacks::inert(),
        web: tuff_web_ui::Web::detached(),
        config_store: detached_config_store(),
        metrics: tuff_core::metrics::Metrics::detached(),
        permissions: tuff_core::permission::Permissions::detached(registries.permissions.nodes()),
        hook_owners: resolved.hook_owners,
        timings: tuff_timings::Timings::detached(),
        backups: tuff_core::storage::backup::jobs::Backups::detached(),
        pregen: tuff_core::world::pregen::Pregen::detached(),
    }
}

fn config_with_keys(
    registries: &tuff_core::registry::Registries,
) -> tuff_core::config::ConfigStore {
    let config = tuff_core::config::ConfigStore::new();
    for key in &registries.config_keys {
        config
            .register(key.clone())
            .expect("feature config keys are valid");
    }
    let _ = config.set_text("tuff.anticheat.enforce", "true");
    let _ = config.set_text("tuff.anticheat.kickThreshold", "1");
    config
}

/// How [`TestWorld::start_loaded`] sets the world up beyond its features.
#[derive(Default)]
struct Options {
    packs: tuff_core::datapack::DataPackConfig,
    simulating: Option<(i32, tuff_core::world::SimulationSettings)>,
    /// `tuff.*` settings for this run.
    settings: Vec<(String, String)>,
    /// Vanilla feature ids left out of the world.
    without: &'static [&'static str],
}

pub type PluginAdvancement = (u128, bool, String, Option<String>);

pub struct TestWorld {
    pub clock: watch::Sender<u64>,
    /// Every server tick; `ticks` moves it with `clock`, `frozen_ticks`
    /// alone, as `/tick freeze` does.
    pub server_ticks: watch::Sender<u64>,
    pub manager: mpsc::Sender<ManagerMessage>,
    pub shared: Arc<Shared>,
    /// Game rules; natural mob spawning starts off so it cannot disturb
    /// tests that count entities.
    pub game_rules: watch::Sender<tuff_core::world::game_rules::GameRules>,
    pub difficulty: watch::Sender<Difficulty>,
    pub clocks: watch::Sender<tuff_core::world::clock::WorldClocks>,
    /// The player index a coordinator would publish.
    pub players: watch::Sender<Arc<tuff_core::world::nearby::PlayerIndex>>,
    /// Chat the coordinator was asked to announce to everyone.
    pub announcements: Arc<std::sync::Mutex<Vec<TextComponent>>>,
    /// Lines the coordinator was asked to send to operators only.
    pub operator_alerts: Arc<std::sync::Mutex<Vec<TextComponent>>>,
    /// Feature teleports the coordinator was asked to run, in order.
    pub teleports: Arc<std::sync::Mutex<Vec<(u128, tuff_core::world::messages::Teleport)>>>,
    /// Feature advancement grants (true) and revokes the coordinator was
    /// asked for, as (player, grant, advancement, criterion).
    pub plugin_advancements: Arc<std::sync::Mutex<Vec<PluginAdvancement>>>,
    /// Players whose sessions reported leaving, in order.
    pub left: Arc<std::sync::Mutex<Vec<tuff_core::world::messages::PlayerId>>>,
    /// How often the coordinator was asked to stop the server.
    pub stops: Arc<std::sync::atomic::AtomicUsize>,
    pub weather: watch::Sender<tuff_core::world::weather::Weather>,
    /// The world debug feed subscribers a coordinator would publish.
    pub debug_subscribers: watch::Sender<tuff_core::world::debug_feeds::SharedSubscribers>,
    /// Each dimension's world border, by dimension ID; `ticks` moves them
    /// and `/worldborder` changes them as the coordinator would.
    pub borders: Arc<Vec<watch::Sender<tuff_core::world::border::Border>>>,
    pub _watchdog: tuff_core::watchdog::Watchdog,
}

impl TestWorld {
    pub fn start(dir: &Path) -> Self {
        Self::start_with(Storage::default().open(dir).unwrap())
    }

    pub fn start_with(backend: Arc<dyn WorldBackend>) -> Self {
        Self::start_generated(backend, placeholder::for_settings)
    }

    pub fn start_generated(
        backend: Arc<dyn WorldBackend>,
        make: fn(&DimensionSettings, i64) -> Arc<dyn ChunkGenerator>,
    ) -> Self {
        Self::start_full(backend, make, false)
    }

    /// A world whose end runs its dragon fight.
    pub fn start_with_fights(dir: &Path) -> Self {
        Self::start_full(
            Storage::default().open(dir).unwrap(),
            placeholder::for_settings,
            true,
        )
    }

    pub fn start_full(
        backend: Arc<dyn WorldBackend>,
        make: fn(&DimensionSettings, i64) -> Arc<dyn ChunkGenerator>,
        with_fights: bool,
    ) -> Self {
        Self::start_loaded(
            backend,
            make,
            with_fights,
            vanilla_registries(),
            Vec::new(),
            Options::default(),
        )
    }

    /// A new world that starts with the data packs `enabled` (besides
    /// vanilla), as `initial-enabled-packs` would give it, so a built-in
    /// feature pack turns its feature flag on.
    pub fn start_with_packs(dir: &Path, enabled: &[&str]) -> Self {
        let mut packs = tuff_core::datapack::DataPackConfig::default();
        packs
            .enabled
            .extend(enabled.iter().map(|id| (*id).to_owned()));
        Self::start_loaded(
            Storage::default().open(dir).unwrap(),
            placeholder::for_settings,
            false,
            vanilla_registries(),
            Vec::new(),
            Options {
                packs,
                ..Options::default()
            },
        )
    }

    /// A world with the given view distance and simulation settings.
    pub fn start_simulating(
        dir: &Path,
        view_distance: i32,
        simulation: tuff_core::world::SimulationSettings,
    ) -> Self {
        Self::start_loaded(
            Storage::default().open(dir).unwrap(),
            placeholder::for_settings,
            false,
            vanilla_registries(),
            Vec::new(),
            Options {
                simulating: Some((view_distance, simulation)),
                ..Options::default()
            },
        )
    }

    /// A world running the vanilla set plus `extra` features, with the
    /// world services only the extra features register (vanilla's dragon
    /// fights stay off, as in [`Self::start`]).
    pub fn start_with_features(
        dir: &Path,
        extra: Vec<Box<dyn tuff_core::feature::Feature>>,
    ) -> Self {
        Self::start_with_features_on(Storage::default().open(dir).unwrap(), extra)
    }

    /// [`Self::start_with_features`] on a given backend.
    pub fn start_with_features_on(
        backend: Arc<dyn WorldBackend>,
        extra: Vec<Box<dyn tuff_core::feature::Feature>>,
    ) -> Self {
        Self::start_generated_with_features(backend, placeholder::for_settings, extra)
    }

    /// [`Self::start_with_features_on`] over a given chunk generator.
    pub fn start_generated_with_features(
        backend: Arc<dyn WorldBackend>,
        make: fn(&DimensionSettings, i64) -> Arc<dyn ChunkGenerator>,
        extra: Vec<Box<dyn tuff_core::feature::Feature>>,
    ) -> Self {
        Self::start_features_options(backend, make, extra, Options::default())
    }

    /// [`Self::start_with_features`] with `tuff.*` settings applied for
    /// this run, as `--config` gives them: the vanilla dimensions they
    /// switch off are left out and the spawn dimension is picked (and the
    /// spawn moved there) as a server would at startup.
    pub fn start_with_settings(
        dir: &Path,
        extra: Vec<Box<dyn tuff_core::feature::Feature>>,
        settings: &[(&str, &str)],
    ) -> Self {
        Self::start_features_options(
            Storage::default().open(dir).unwrap(),
            placeholder::for_settings,
            extra,
            Options {
                settings: settings
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                    .collect(),
                ..Options::default()
            },
        )
    }

    /// [`Self::start_with_features`] without `vanilla-survival`, so the
    /// advancement framework runs on what `extra` registers alone.
    pub fn start_without_survival(
        dir: &Path,
        extra: Vec<Box<dyn tuff_core::feature::Feature>>,
    ) -> Self {
        Self::start_features_options(
            Storage::default().open(dir).unwrap(),
            placeholder::for_settings,
            extra,
            Options {
                without: &["vanilla-survival"],
                ..Options::default()
            },
        )
    }

    /// [`Self::start_without_survival`] on a given backend.
    pub fn start_without_survival_on(
        backend: Arc<dyn WorldBackend>,
        extra: Vec<Box<dyn tuff_core::feature::Feature>>,
    ) -> Self {
        Self::start_features_options(
            backend,
            placeholder::for_settings,
            extra,
            Options {
                without: &["vanilla-survival"],
                ..Options::default()
            },
        )
    }

    fn start_features_options(
        backend: Arc<dyn WorldBackend>,
        make: fn(&DimensionSettings, i64) -> Arc<dyn ChunkGenerator>,
        extra: Vec<Box<dyn tuff_core::feature::Feature>>,
        options: Options,
    ) -> Self {
        let mut features = vanilla_features();
        features.retain(|feature| !options.without.contains(&feature.id()));
        features.extend(extra);
        let mut registries = tuff_core::registry::Registries::new();
        tuff_core::feature::load_all(&features, &mut registries).expect("the features load");
        let services = registries.services[vanilla_registries().services.len()..].to_vec();
        Self::start_loaded(
            backend,
            make,
            false,
            Arc::new(registries),
            services,
            options,
        )
    }

    fn start_loaded(
        backend: Arc<dyn WorldBackend>,
        make: fn(&DimensionSettings, i64) -> Arc<dyn ChunkGenerator>,
        with_fights: bool,
        registries: Arc<tuff_core::registry::Registries>,
        mut factories: Vec<Arc<dyn tuff_core::world::services::ServiceFactory>>,
        options: Options,
    ) -> Self {
        let Options {
            packs,
            simulating,
            settings,
            without: _,
        } = options;
        let config = config_with_keys(&registries);
        for (key, value) in &settings {
            config
                .set_text_for_run(key, value)
                .unwrap_or_else(|err| panic!("setting {key} {value}: {err}"));
        }
        let (clock_tx, clock) = watch::channel(1);
        let (server_ticks_tx, server_ticks) = watch::channel(1);
        let (manager, manager_rx) = mpsc::channel(64);
        let (coordinator, mut coordinator_rx) = mpsc::channel(64);
        // Nothing acts on coordinator messages here; drain them so sessions
        // under test never block on a full queue, keeping announcements.
        let announcements = Arc::new(std::sync::Mutex::new(Vec::new()));
        let heard = announcements.clone();
        let operator_alerts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let alerted = operator_alerts.clone();
        let teleports = Arc::new(std::sync::Mutex::new(Vec::new()));
        let teleported = teleports.clone();
        let plugin_advancements = Arc::new(std::sync::Mutex::new(Vec::new()));
        let advanced = plugin_advancements.clone();
        let left = Arc::new(std::sync::Mutex::new(Vec::new()));
        let departed = left.clone();
        let stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stopped = stops.clone();
        let regions = manager.clone();
        let journal = backend.save_journal();
        let provider_context = tuff_core::world::custom_dimension::ProviderContext {
            config_store: backend.config().unwrap_or_else(detached_config_store),
            settings: config.clone(),
            properties: Default::default(),
        };
        for provider in &registries.dimension_providers {
            provider.attach(&provider_context);
        }
        let enabled =
            tuff_core::world::dimension::enabled_settings(&DimensionSettings::vanilla(), &config);
        let mut dimensions = Dimensions::from_settings_with(&enabled, 1, make)
            .with_custom(plugin_dimensions(&registries), 1);
        assert!(
            dimensions.iter().len() > 0,
            "{}",
            tuff_core::world::NO_DIMENSIONS
        );
        let borders = Arc::new(dimensions.take_border_writers());
        let border_owner = borders.clone();
        tokio::spawn(async move {
            while let Some(message) = coordinator_rx.recv().await {
                match message {
                    CoordinatorMessage::WorldBorder {
                        dimension,
                        change,
                        reply,
                    } => {
                        let mut result = Ok(None);
                        border_owner[dimension.index()].send_if_modified(|border| {
                            result = border.apply(change);
                            result.is_ok()
                        });
                        let _ = reply.send(result);
                    }
                    CoordinatorMessage::Announce(text)
                    | CoordinatorMessage::Tell { message: text, .. } => {
                        heard.lock().unwrap().push(text)
                    }
                    CoordinatorMessage::OperatorAlert { line, .. } => {
                        alerted.lock().unwrap().push(line)
                    }
                    CoordinatorMessage::Sync { reply } => {
                        let _ = reply.send(());
                    }
                    CoordinatorMessage::TeleportPlayer { uuid, teleport } => {
                        teleported.lock().unwrap().push((uuid, teleport))
                    }
                    CoordinatorMessage::PluginAdvancement {
                        uuid,
                        grant,
                        advancement,
                        criterion,
                    } => advanced
                        .lock()
                        .unwrap()
                        .push((uuid, grant, advancement, criterion)),
                    CoordinatorMessage::Left { player } => departed.lock().unwrap().push(player),
                    CoordinatorMessage::Stop => {
                        stopped.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                    CoordinatorMessage::SaveAll { flush, done } => {
                        saves::save_all(regions.clone(), journal.as_ref(), flush, done)
                    }
                    other => tuff_item::item::conservation::release(other),
                }
            }
        });
        let mut spawn = Spawn {
            dimension: OVERWORLD.into(),
            pos: BlockPos::new(0, 80, 0),
            yaw: 0.0,
            pitch: 0.0,
        };
        let chosen = dimensions.choose_spawn(
            &config
                .get_string(tuff_core::world::dimension::SPAWN_DIMENSION_KEY)
                .unwrap_or_default(),
        );
        dimensions.place_spawn(chosen, &mut spawn);
        let metrics =
            tuff_core::metrics::Metrics::spawn(tuff_core::metrics::MetricsConfig::by_ticks());
        let chunk_io =
            tuff_core::world::chunk_io::ChunkIo::start_metered(backend.clone(), metrics.clone());
        let backend = chunk_io.backend().clone();
        let config_store = backend.config().unwrap_or_else(detached_config_store);
        let mut rules = tuff_core::world::game_rules::GameRules::default();
        rules
            .set(
                "spawn_mobs",
                tuff_core::world::game_rules::RuleValue::Bool(false),
            )
            .expect("a boolean rule");
        // Off by default too, for the same reason: phantoms have their own
        // dedicated spawn timer that fires on a region's very first tick
        // regardless, which would otherwise draw from the region's RNG in
        // every test and perturb other tests' expected random outcomes.
        rules
            .set(
                "spawn_phantoms",
                tuff_core::world::game_rules::RuleValue::Bool(false),
            )
            .expect("a boolean rule");
        let game_rules = watch::Sender::new(rules);
        let difficulty = watch::Sender::new(Difficulty::Normal);
        let clocks = watch::Sender::new(tuff_core::world::clock::WorldClocks::default());
        let tick_state = watch::Sender::new(tuff_core::world::tick_rate::TickState::default());
        let players = watch::Sender::new(Arc::default());
        let weather = watch::Sender::new(tuff_core::world::weather::Weather::default());
        let debug_subscribers = watch::Sender::new(Default::default());
        if with_fights {
            factories.push(Arc::new(
                tuff_feature_builtin_vanilla_dimensions::dragon::fight::FightsFactory,
            ));
        }
        factories.push(Arc::new(
            tuff_feature_builtin_vanilla_maps::service::MapServiceFactory,
        ));
        factories.push(Arc::new(
            tuff_feature_builtin_vanilla_maps::ExplorationFactory,
        ));
        factories.push(Arc::new(
            tuff_feature_builtin_vanilla_worldgen::feature::host::FeatureServiceFactory,
        ));
        factories.push(Arc::new(
            tuff_feature_builtin_vanilla_commands::function::FunctionTickerFactory,
        ));
        factories.push(Arc::new(
            tuff_feature_builtin_vanilla_commands::bossbar::BossBarFactory,
        ));
        factories.push(Arc::new(
            tuff_feature_builtin_vanilla_commands::gametest::GameTestsFactory,
        ));
        factories.push(Arc::new(
            tuff_feature_builtin_vanilla_anticheat::service::BoardFactory,
        ));
        factories.push(Arc::new(
            tuff_feature_builtin_vanilla_mobs::world::raid_store::RaidStoreFactory,
        ));
        factories.push(Arc::new(
            tuff_feature_builtin_vanilla_mobs::world::trader_store::TraderStoreFactory,
        ));
        factories.push(Arc::new(
            tuff_feature_builtin_vanilla_mobs::world::spawn_census::SpawnCensusFactory,
        ));
        let services = tuff_core::world::services::Services::create(&factories, &dimensions);
        let resolved =
            Resolved::from_registries(&registries).with_provided(&registries, &provider_context);
        let (scoreboard, _) = tuff_core::world::scoreboard::start(backend.clone(), clock.clone());
        let (command_storage, _) = tuff_core::world::command_storage::start(backend.clone());
        let datapacks = tuff_core::datapack::DataPacks::open_now(
            backend.clone(),
            resolved.commands.clone(),
            tuff_core::datapack::Base::new(&resolved),
            packs,
            None,
        );
        game_rules.send_modify(|rules| rules.apply_features(&datapacks.features()));
        let generation = tuff_core::world::generator::start_pipelines(
            &dimensions,
            &server_ticks,
            Some((&chunk_io, &backend.chunk_codec())),
            &tuff_datapack::tags::TagFeed::new(datapacks.watch_contents()),
        );
        let dimension_names = dimensions.iter().map(|d| d.key.clone()).collect();
        let shared = Arc::new(Shared {
            conservation: Default::default(),
            stopwatches: Default::default(),
            random_sequences: Default::default(),
            server_list: std::sync::OnceLock::new(),
            live: tuff_core::world::live::LiveSettings::new(
                simulating.map_or(2, |s| s.0),
                10,
                play::GameMode::Survival,
                simulating.map_or_else(Default::default, |s| s.1),
                Default::default(),
            ),
            chunk_io: tuff_core::world::chunk_io::ChunkIoCell::with(chunk_io),
            backend,
            world_path: None,
            dimensions,
            online_mode: false,
            spawn: watch::channel(spawn).1,
            difficulty: difficulty.subscribe(),
            game_rules: game_rules.subscribe(),
            weather: weather.subscribe(),
            debug_subscribers: debug_subscribers.subscribe(),
            seed: 1,
            spawn_protection: Default::default(),
            hardcore: Default::default(),
            difficulty_locked: Default::default(),
            ops_present: Default::default(),
            text_filter: None,
            force_game_mode: Default::default(),
            profile_keys: Default::default(),
            clock,
            server_ticks,
            clocks: clocks.subscribe(),
            tick_state: tick_state.subscribe(),
            players: players.subscribe(),
            manager: manager.clone(),
            coordinator,
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
            tracer: tuff_trace::Tracer::inert(),
            random_seed: Some(1),
            entity_compute_shuffle: Default::default(),
            entity_compute_min_batch: Default::default(),
            entity_ids: AtomicI32::new(1),
            entity_uuids: Default::default(),
            tick_barrier: tuff_core::world::tick_barrier::TickBarrier::new(Default::default()),
            scoreboard,
            command_storage,
            config: config.clone(),
            http: tuff_core::http::spawn_metered(&metrics),
            public_base_url: tuff_core::public_ip::BaseUrl::detached("http://127.0.0.1:0"),
            resource_packs: tuff_core::resource_pack::ResourcePacks::spawn(
                tuff_core::public_ip::BaseUrl::detached("http://127.0.0.1:0"),
                config.clone(),
            ),
            custom: resolved.custom.clone(),
            custom_pack: Default::default(),
            datapacks,
            config_store,
            web: tuff_web_ui::Web {
                events: tuff_web_ui::events::PanelEvents::spawn(),
                live: tuff_web_ui::live::LivePanel::spawn(),
                ..tuff_web_ui::Web::detached()
            },
            metrics,
            permissions: tuff_core::permission::Permissions::start(registries.permissions.nodes()),
            timings: tuff_timings::Timings::spawn(
                None,
                resolved.hook_owners.features.clone(),
                tuff_timings::Labels {
                    dimensions: dimension_names,
                    server_name: "tuff".to_owned(),
                },
            ),
            hook_owners: resolved.hook_owners,
            backups: tuff_core::storage::backup::jobs::Backups::spawn(
                tuff_core::storage::backup::jobs::BackupsConfig {
                    instance_dir: None,
                    level: "world".to_owned(),
                    settings: config.clone(),
                },
            ),
            pregen: tuff_core::world::pregen::Pregen::spawn(),
        });
        shared.backups.attach(&shared);
        tokio::spawn(RegionManager::new(shared.clone(), manager_rx).run());
        shared.services.start(&shared);
        Self {
            clock: clock_tx,
            server_ticks: server_ticks_tx,
            manager,
            shared,
            game_rules,
            difficulty,
            clocks,
            players,
            announcements,
            operator_alerts,
            teleports,
            plugin_advancements,
            left,
            stops,
            weather,
            debug_subscribers,
            borders,
            _watchdog: tuff_core::watchdog::arm(tuff_core::watchdog::DEFAULT),
        }
    }

    /// Everything announced so far, once the coordinator queue has drained.
    pub async fn announced(&self) -> Vec<TextComponent> {
        let (reply, done) = oneshot::channel();
        self.shared
            .coordinator
            .send(CoordinatorMessage::Sync { reply })
            .await
            .unwrap();
        done.await.unwrap();
        self.announcements.lock().unwrap().clone()
    }

    /// Publishes the player index a coordinator would, so regions without
    /// the players' presence still see them (spawning, despawning, targets).
    pub fn publish_players(&self, dimension: &str, players: &[(&PlayerHandle, [f64; 3])]) {
        let dimension = self.dimension(dimension);
        let players = players.iter().map(|(handle, position)| {
            (
                dimension,
                tuff_core::world::nearby::IndexedPlayer {
                    handle: (*handle).clone(),
                    uuid: handle.id as u128,
                    name: Arc::from(format!("player{}", handle.id)),
                    position: *position,
                    looks: Default::default(),
                    time_since_rest: 0,
                },
            )
        });
        self.players
            .send_replace(Arc::new(tuff_core::world::nearby::PlayerIndex::build(
                players,
            )));
    }

    pub async fn region(&self, region: RegionPos) -> RegionHandle {
        self.region_in(OVERWORLD, region).await
    }

    pub async fn region_in(&self, dimension: &str, region: RegionPos) -> RegionHandle {
        let dimension = self.dimension(dimension);
        let (reply, handle) = oneshot::channel();
        self.manager
            .send(ManagerMessage::Acquire {
                dimension,
                region,
                reply,
            })
            .await
            .unwrap();
        tokio::time::timeout(TIMEOUT, handle)
            .await
            .expect("the manager answers")
            .unwrap()
    }

    pub fn dimension(&self, key: &str) -> DimensionId {
        self.shared.dimensions.by_key(key).expect("dimension").id
    }

    /// Runs `count` ticks, each one only after every region and fight
    /// has run the previous one and landed the jobs it started, so a test
    /// sees exactly the ticks it asks for however loaded the machine is.
    pub async fn ticks(&self, count: u64) {
        for _ in 0..count {
            for border in self.borders.iter() {
                border.send_if_modified(tuff_core::world::border::Border::tick);
            }
            self.clock.send_modify(|tick| *tick += 1);
            self.server_ticks.send_modify(|tick| *tick += 1);
            let tick = *self.clock.borrow();
            if tick.is_multiple_of(6000) && self.shared.saves().autosaves() {
                let _ = self.manager.try_send(ManagerMessage::SaveAll {
                    done: None,
                    snapshotted: None,
                    epoch: None,
                });
            }
            self.settle(tick).await;
        }
    }

    /// Runs `count` server ticks while the game is frozen: the game clock
    /// holds still. Settles on idleness only, as no game tick is due.
    pub async fn frozen_ticks(&self, count: u64) {
        for _ in 0..count {
            self.server_ticks.send_modify(|tick| *tick += 1);
            self.settle(0).await;
        }
    }

    pub async fn settle(&self, tick: u64) {
        let settled = tokio::time::timeout(TICK_TIMEOUT, async {
            for _ in 0..2 {
                self.shared.services.settled(tick).await;
                let (reply, regions) = oneshot::channel();
                self.manager
                    .send(ManagerMessage::Regions { reply })
                    .await
                    .unwrap();
                let mut answers = Vec::new();
                for region in regions.await.unwrap() {
                    let (reply, answer) = oneshot::channel();
                    region.request(RegionRequest::Settled { tick, reply }).await;
                    answers.push(answer);
                }
                for answer in answers {
                    let _ = answer.await;
                }
            }
        })
        .await;
        assert!(
            settled.is_ok(),
            "tick {tick} did not settle within {TICK_TIMEOUT:?}"
        );
    }

    pub async fn shutdown(self) {
        let (done, finished) = oneshot::channel();
        self.manager
            .send(ManagerMessage::Shutdown { done })
            .await
            .unwrap();
        tokio::time::timeout(TICK_TIMEOUT, finished)
            .await
            .expect("every region closes")
            .unwrap();
        if let Some(journal) = self.shared.backend.save_journal() {
            journal.finish();
        }
    }
}

pub async fn expect<T>(
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    mut matches: impl FnMut(PlayerEvent) -> Option<T>,
) -> T {
    let mut skipped: Vec<String> = Vec::new();
    let result = tokio::time::timeout(TIMEOUT, async {
        loop {
            let event = events.recv().await.expect("events closed");
            let mut summary = format!("{event:?}");
            summary.truncate(160);
            if let Some(found) = matches(event) {
                return found;
            }
            skipped.push(summary);
        }
    })
    .await;
    match result {
        Ok(found) => found,
        Err(_) => panic!(
            "timed out waiting for event; skipped {} events, the last of which were {:#?}",
            skipped.len(),
            &skipped[skipped.len().saturating_sub(12)..]
        ),
    }
}

pub async fn subscribe(
    region: &RegionHandle,
    viewer: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    chunks: &[ChunkPos],
) {
    region
        .request(RegionRequest::Subscribe {
            player: viewer.clone(),
            chunks: chunks.to_vec(),
        })
        .await;
    let mut pending: Vec<ChunkPos> = chunks.to_vec();
    while !pending.is_empty() {
        let pos = expect(events, |event| match event {
            PlayerEvent::Chunk { pos, .. } => Some(pos),
            _ => None,
        })
        .await;
        pending.retain(|p| *p != pos);
    }
}

pub fn stored_chunk(dir: &Path, pos: ChunkPos, entities: bool) -> Option<Compound> {
    let paths = VanillaBackend::new(dir);
    let paths = paths.paths();
    let folder = if entities {
        paths.entities_dir(OVERWORLD)
    } else {
        paths.region_dir(OVERWORLD)
    };
    let region = pos.file_region();
    let mut file = RegionFile::open(&folder, region.x, region.z, false).unwrap()?;
    let raw = file.read(pos).unwrap()?;
    let bytes = raw.decompress(1 << 24).unwrap();
    match NamedTag::read(&mut &bytes[..], NbtLimits::TRUSTED)
        .unwrap()
        .tag
    {
        Tag::Compound(root) => Some(root),
        _ => None,
    }
}

pub fn decode_stored(
    root: Compound,
    pos: ChunkPos,
) -> Result<Decoded<tuff_core::world::block_entity::BlockEntity>, tuff_storage::error::ChunkNbtError>
{
    chunk_nbt::decode(root, pos, ChunkShape::OVERWORLD)
}

pub fn update(player: &PlayerHandle, seq: u64, position: [f64; 3]) -> RegionRequest {
    RegionRequest::Player(PlayerUpdate {
        looks: Default::default(),
        handle: player.clone(),
        uuid: u128::from(player.id as u32),
        name: format!("Player{}", player.id),
        seq,
        position,
        swept_from: position,
        chunk: ChunkPos::of_position(position),
        alive: true,
        spectator: false,
        gamemaster: false,
        explosion_knockback_resistance: 0.0,
        creative_flying: false,
        grounded: false,
        feet: None,
        landed: None,
        spawn: packets([]),
        update: packets([]),
        head: None,
        moves: None,
    })
}

/// Decodes a framed Block Update packet.
pub fn decode_block_update(
    frame: &bytes::Bytes,
) -> Option<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut r = framed_payload(frame);
    if r.read_var_int().ok()? != tuff_vanilla_data::packet::play::Clientbound::BlockUpdate.id() {
        return None;
    }
    let position = r.read_position().ok()?;
    let state = tuff_vanilla_data::BlockState::from_id(r.read_var_int().ok()? as u32)?;
    Some((position, state))
}

/// Decodes a framed Sound packet: the sound's registry id, category,
/// volume and pitch.
pub struct DecodedSound {
    pub sound_event: u32,
    pub category: i32,
    pub volume: f32,
    pub pitch: f32,
}

pub fn decode_sound(frame: &bytes::Bytes) -> Option<DecodedSound> {
    let mut r = framed_payload(frame);
    if r.read_var_int().ok()? != tuff_vanilla_data::packet::play::Clientbound::Sound.id() {
        return None;
    }
    let sound_event = r.read_var_int().ok()? as u32 - 1;
    let category = r.read_var_int().ok()?;
    for _ in 0..3 {
        r.read_int().ok()?;
    }
    let volume = r.read_float().ok()?;
    let pitch = r.read_float().ok()?;
    Some(DecodedSound {
        sound_event,
        category,
        volume,
        pitch,
    })
}

pub const EXPORT_ENV: &str = "MC_VANILLA_COMPAT_WORLD";
pub const EXPORT_SEED: i64 = 1;

/// The decompressed payload (packet id and fields) of a framed packet.
pub fn framed_payload(frame: &bytes::Bytes) -> tuff_wire::PacketReader {
    use tuff_nbt::Compression;
    use tuff_wire::PacketReader;
    let mut r = PacketReader::new(frame.clone());
    r.read_var_int().unwrap();
    let data_len = r.read_var_int().unwrap();
    let rest = r.read_bytes(r.remaining()).unwrap();
    let payload = if data_len == 0 {
        rest
    } else {
        bytes::Bytes::from(
            Compression::Zlib
                .decompress(&rest, data_len as usize)
                .unwrap()
                .into_owned(),
        )
    };
    PacketReader::new(payload)
}

pub fn framed_packet_id(frame: &bytes::Bytes) -> i32 {
    framed_payload(frame).read_var_int().unwrap()
}

/// Places blocks through the region's cross-border path, which applies
/// them on the next tick.
pub async fn place_blocks(
    world: &TestWorld,
    region: &RegionHandle,
    blocks: impl IntoIterator<Item = (BlockPos, tuff_vanilla_data::BlockState)>,
) {
    let updates = blocks
        .into_iter()
        .map(|(pos, state)| BlockUpdate::Place {
            pos,
            state,
            cause: Default::default(),
        })
        .collect();
    region.internal(RegionInternal::BlockUpdates {
        tick: None,
        updates,
    });
    world.ticks(2).await;
}

pub async fn block_at(
    region: &RegionHandle,
    pos: BlockPos,
) -> Option<tuff_vanilla_data::BlockState> {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::BlockAt { pos, reply }).await;
    answer.await.unwrap()
}

pub fn container_menu(event: PlayerEvent) -> Option<MenuOpen> {
    match event {
        PlayerEvent::MenuOpen(open) => Some(open),
        _ => None,
    }
}

pub fn transaction_answer(event: PlayerEvent) -> TransactionAnswer {
    match event {
        PlayerEvent::Transaction {
            accepted, contents, ..
        } => Some((accepted, contents)),
        _ => None,
    }
}

/// Stacks for what `changes` add, as the player's side of a transaction
/// gives them.
pub fn pool_for(changes: &[ContainerChange]) -> Vec<ItemStack> {
    gains(
        changes
            .iter()
            .map(|c| (c.expected.as_ref(), c.new.as_ref())),
    )
}

/// [`pool_for`] for an entity container.
pub fn entity_pool_for(changes: &[tuff_core::world::messages::EntitySlotChange]) -> Vec<ItemStack> {
    gains(
        changes
            .iter()
            .map(|c| (c.expected.as_ref(), c.new.as_ref())),
    )
}

fn gains<'a>(
    changes: impl Iterator<Item = (Option<&'a ItemSnapshot>, Option<&'a ItemSnapshot>)>,
) -> Vec<ItemStack> {
    changes
        .filter_map(|(expected, new)| {
            tuff_core::world::menu::settle::delta(expected.map(|s| &**s), new.map(|s| &**s)).1
        })
        .map(|shown| ItemStack::mint_like(&shown, shown.count(), MintReason::Test))
        .collect()
}

/// A picture of a test stack, for a transaction's expected or new slot.
pub fn snap<S: std::borrow::Borrow<ItemStack>>(stack: Option<S>) -> Option<ItemSnapshot> {
    stack.as_ref().map(|stack| ItemSnapshot::of(stack.borrow()))
}

/// Test stacks showing what `pictures` show, to compare with.
pub fn stacks(pictures: &[Option<ItemSnapshot>]) -> Vec<Option<ItemStack>> {
    pictures
        .iter()
        .map(|slot| {
            slot.as_ref()
                .map(|shown| ItemStack::mint_like(shown, shown.count(), MintReason::Test))
        })
        .collect()
}

/// A transaction changing one slot, from pictures of what it holds and
/// should hold.
pub fn set_slot(
    pos: BlockPos,
    slot: usize,
    expected: Option<ItemStack>,
    new: Option<ItemStack>,
) -> Vec<ContainerChange> {
    vec![ContainerChange {
        pos,
        slot,
        expected: expected.as_ref().map(ItemSnapshot::of),
        new: new.as_ref().map(ItemSnapshot::of),
    }]
}

pub fn spawned_entity_kind(track: &tuff_core::world::messages::Track) -> i32 {
    let mut r = framed_payload(&track.spawn[0]);
    r.read_var_int().unwrap();
    r.read_var_int().unwrap();
    r.read_uuid().unwrap();
    r.read_var_int().unwrap()
}

/// The block entity list of a framed Chunk Data packet, as
/// `(packed xz, y, type id, nbt)`.
pub fn chunk_block_entities(frame: &bytes::Bytes) -> Vec<(u8, i16, i32, Option<Tag>)> {
    let mut r = framed_payload(frame);
    assert_eq!(
        r.read_var_int().unwrap(),
        tuff_vanilla_data::packet::play::Clientbound::LevelChunkWithLight.id()
    );
    r.read_int().unwrap();
    r.read_int().unwrap();
    for _ in 0..r.read_var_int().unwrap() {
        r.read_var_int().unwrap();
        for _ in 0..r.read_var_int().unwrap() {
            r.read_long().unwrap();
        }
    }
    let sections = r.read_var_int().unwrap() as usize;
    r.read_bytes(sections).unwrap();
    (0..r.read_var_int().unwrap())
        .map(|_| {
            (
                r.read_unsigned_byte().unwrap(),
                r.read_short().unwrap(),
                r.read_var_int().unwrap(),
                r.read_nbt().unwrap(),
            )
        })
        .collect()
}

pub async fn chunk_packet_for(
    region: &RegionHandle,
    viewer: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    chunk: ChunkPos,
) -> bytes::Bytes {
    region
        .request(RegionRequest::Subscribe {
            player: viewer.clone(),
            chunks: vec![chunk],
        })
        .await;
    expect(events, |event| match event {
        PlayerEvent::Chunk { pos, packet, .. } if pos == chunk => Some(packet),
        _ => None,
    })
    .await
}

/// Loads the chunks around `center` (a 3x3), so fluid and block ticks
/// there may run.
/// An area of chunks viewed through every region that owns part of it,
/// for scenarios larger than one region.
pub struct Area {
    regions: Vec<(RegionPos, RegionHandle)>,
}

impl Area {
    /// Subscribes `viewer` to `chunks` in the regions that own them.
    pub async fn subscribe(
        world: &TestWorld,
        viewer: &PlayerHandle,
        events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
        chunks: &[ChunkPos],
    ) -> Self {
        let mut by_region: Vec<(RegionPos, Vec<ChunkPos>)> = Vec::new();
        for chunk in chunks {
            match by_region.iter_mut().find(|(r, _)| *r == chunk.region()) {
                Some((_, owned)) => owned.push(*chunk),
                None => by_region.push((chunk.region(), vec![*chunk])),
            }
        }
        let mut regions = Vec::new();
        for (pos, owned) in by_region {
            let region = world.region(pos).await;
            subscribe(&region, viewer, events, &owned).await;
            regions.push((pos, region));
        }
        Self { regions }
    }

    /// The region owning `position`, which must be in the area.
    pub fn owner(&self, position: [f64; 3]) -> &RegionHandle {
        let pos = ChunkPos::of_position(position).region();
        &self
            .regions
            .iter()
            .find(|(r, _)| *r == pos)
            .expect("the position is in the area")
            .1
    }

    pub fn regions(&self) -> impl Iterator<Item = &RegionHandle> {
        self.regions.iter().map(|(_, region)| region)
    }

    /// Every entity in the area's regions.
    pub async fn entities(&self) -> Vec<tuff_core::world::messages::EntitySnapshot> {
        let mut all = Vec::new();
        for region in self.regions() {
            let (reply, answer) = oneshot::channel();
            region.request(RegionRequest::Entities { reply }).await;
            all.extend(answer.await.unwrap());
        }
        all
    }
}

pub async fn view_around(
    world: &TestWorld,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    center: ChunkPos,
) {
    let mut by_region: std::collections::HashMap<RegionPos, Vec<ChunkPos>> = Default::default();
    for dx in -1..=1 {
        for dz in -1..=1 {
            let chunk = ChunkPos::new(center.x + dx, center.z + dz);
            by_region.entry(chunk.region()).or_default().push(chunk);
        }
    }
    for (region, chunks) in by_region {
        let region = world.region(region).await;
        subscribe(&region, player, events, &chunks).await;
    }
}

/// Runs `f` with a tick context over a flat grass world at y 64.
pub fn with_tick_context(f: impl FnOnce(&mut tuff_core::world::entity::TickContext)) {
    with_tick_context_around(&[], f);
}

/// Like [`with_tick_context`], with `entities` standing in the region.
pub fn with_tick_context_around(
    entities: &[tuff_core::world::entity::EntitySummary],
    f: impl FnOnce(&mut tuff_core::world::entity::TickContext),
) {
    let shared = bare_shared();
    let block = |pos: BlockPos| {
        Some(if pos.y < 64 {
            Block::GrassBlock.default_state()
        } else {
            Block::Air.default_state()
        })
    };
    let light = |_: BlockPos| Some((15, 0));
    let biome = |_: BlockPos| Some("minecraft:plains");
    let mut rng = StdRng::seed_from_u64(1);
    let mut effects = Vec::new();
    let mut budget = 10_000;
    let pois = tuff_core::world::poi::PoiStore::default();
    let mut cx = tuff_core::world::entity::TickContext {
        tick: 1,
        dimension: DimensionId::for_tests(0),
        block: &block,
        light: &light,
        sky_darken: 0,
        ambient_light: 0.0,
        monsters_burn: true,
        raining: false,
        biome: &biome,
        container: &|_| None,
        container_info: &|_| Default::default(),
        inhabited_time: &|_| 0,
        min_y: -64,
        max_y: 319,
        players: &[],
        players_in_dimension: 0,
        entities,
        entity_grid: None,
        pistons: &[],
        parts: &[],
        shared: &shared,
        rng: &mut rng,
        effects: &mut effects,
        path_budget: &mut budget,
        pois: &pois,
        state: &tuff_core::world::region_api::RegionStateLock::default(),
    };
    f(&mut cx);
}

/// Runs `sql` on the SQLite file at `path` as an outside tool would. Kept
/// out of line so tests reach SQLite through the same copy the server
/// links, also when the root binaries link the dylib facade.
#[inline(never)]
pub fn sqlite_execute(path: &Path, sql: &str) -> rusqlite::Result<usize> {
    rusqlite::Connection::open(path)?.execute(sql, [])
}
