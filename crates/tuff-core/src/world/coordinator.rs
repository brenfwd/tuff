//! The one piece of genuinely global state: who is online (joins, duplicate
//! logins, the tab list, chat), the game clock that every region and player
//! ticks on, `level.dat` and the world-wide settings commands change (game
//! rules, weather, difficulty, spawn), and the operator, whitelist and ban
//! lists. None of this is on a per-tick hot path, so a single actor does
//! not limit scaling.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use rand::SeedableRng;
use rand::rngs::StdRng;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::auth::GameProfile;
use crate::protocol::Encode;
use crate::protocol::play::{self, PlayerInfoEntry, game_event};
use crate::status::{OnlinePlayer, StatusCommand};
use crate::storage::level::{LevelData, Spawn};
use crate::world::border::{Border, BorderChange, BorderError, Resized};
use crate::world::clock::ClockAction;
use crate::world::clock::{ClockId, WorldClocks};
use crate::world::config_actor::{ConfigHandle, ConfigRequest};
use crate::world::dimension::DimensionId;
use crate::world::game_rules::{GameRules, RuleValue};
use crate::world::messages::{
    ChatAudience, ClockOutcome, CoordinatorMessage, DifficultyOutcome, Directive, DirectiveSource,
    FeedbackTarget, GameRuleOutcome, Joined, ListOp, ListOutcome, ManagerMessage, PhaseRequest,
    PlayerEvent, PlayerHandle, PlayerId, PlayerSnapshot, PlayerState, ProfileQuery, Roster,
    RosterEntry, TickOutcome,
};
use crate::world::player::Difficulty;
use crate::world::session::PlayerSession;
use crate::world::tick_barrier::TickLockstep;
use crate::world::tick_rate::TickAction;
use crate::world::tick_rate::{Samples, Sprint, SprintReport, TickState, TickTime, sprint_report};
use crate::world::weather::Weather;
use crate::world::{Shared, framed, registry_id};
use tuff_item::text::TextComponent;
use tuff_storage::admin::PermissionLevel;
use tuff_storage::admin::{BanEntry, NameAndId, OpEntry, ServerProperties, UserLists};
use tuff_storage::backend::WorldLock;
use tuff_supervise::{CatchUnwind, Critical};

const TIME_SYNC_TICKS: u64 = 20;
const LATENCY_SYNC_TICKS: u64 = 600;
const ENTITY_TRANSMITTER_POLL_TICKS: u64 = 20;
const AUTOSAVE_TICKS: u64 = 6000;
/// How long a save waits for sessions and regions to reach its barrier.
const SAVE_BARRIER_WAIT: Duration = Duration::from_secs(1);
const CATCH_UP_LIMIT: std::time::Duration = std::time::Duration::from_secs(2);
/// Client packets queued per player before its socket stops being read.
const PACKET_QUEUE: usize = 256;
pub const SERVER_BRAND: &str = "tuff";

struct Online {
    profile: GameProfile,
    outbound: mpsc::UnboundedSender<Bytes>,
    handle: PlayerHandle,
    /// Set once the player's Login has been sent.
    entry: Option<PlayerInfoEntry>,
    listed: bool,
    latency_dirty: bool,
    address: IpAddr,
    permission: PermissionLevel,
    state: Option<PlayerState>,
    brand: Option<String>,
    /// The `debug_subscription` ids asked for, as a mask; tick time is sent
    /// only while it holds [`crate::permission::DEBUG_TICK_TIME`], world
    /// feeds while it holds [`crate::permission::DEBUG_SUBSCRIPTIONS`].
    debug: u32,
    /// The owner of a fake player; real connections have none.
    fake: Option<String>,
}

/// Stands in for the world lock once it has been released.
struct Released;

const CHAT_VISIBILITY_FULL: i32 = 0;
const CHAT_VISIBILITY_HIDDEN: i32 = 2;

fn chat_mode_shows_player_chat(mode: i32) -> bool {
    mode == CHAT_VISIBILITY_FULL
}

fn chat_mode_shows_system_chat(mode: i32) -> bool {
    mode != CHAT_VISIBILITY_HIDDEN
}

fn shows_player_chat(online: &Online) -> bool {
    online
        .entry
        .as_ref()
        .is_some_and(|e| chat_mode_shows_player_chat(e.chat_mode))
}

fn shows_system_chat(online: &Online) -> bool {
    online
        .entry
        .as_ref()
        .is_some_and(|e| chat_mode_shows_system_chat(e.chat_mode))
}

#[cfg(test)]
mod chat_visibility_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn chat_settings_filter_player_and_system_chat() {
        assert!(chat_mode_shows_player_chat(0));
        assert!(!chat_mode_shows_player_chat(1));
        assert!(!chat_mode_shows_player_chat(2));
        assert!(chat_mode_shows_system_chat(0));
        assert!(chat_mode_shows_system_chat(1));
        assert!(!chat_mode_shows_system_chat(2));
    }
}

impl WorldLock for Released {}

pub struct CoordinatorConfig {
    pub shared: Arc<Shared>,
    pub messages: mpsc::Receiver<CoordinatorMessage>,
    pub clock: watch::Sender<u64>,
    pub server_ticks: watch::Sender<u64>,
    pub clocks: watch::Sender<WorldClocks>,
    pub tick_state: watch::Sender<TickState>,
    pub spawn: watch::Sender<Spawn>,
    pub difficulty: watch::Sender<Difficulty>,
    pub game_rules: watch::Sender<GameRules>,
    pub weather: watch::Sender<Weather>,
    pub debug_subscribers: watch::Sender<crate::world::debug_feeds::SharedSubscribers>,
    /// Each dimension's world border, by dimension ID.
    pub borders: Vec<watch::Sender<Border>>,
    /// Where each finished tick's duration is published.
    pub tick_times: broadcast::Sender<TickTime>,
    /// The player index regions read.
    pub players: watch::Sender<Arc<crate::world::nearby::PlayerIndex>>,
    pub level: LevelData,
    pub level_name: String,
    pub status: mpsc::Sender<StatusCommand>,
    pub lock: Box<dyn WorldLock>,
    pub properties: ServerProperties,
    /// Command-line overridden property keys and the saved value each had
    /// (None when the file did not set it); saves write these back.
    pub property_baseline: std::collections::BTreeMap<String, Option<String>>,
    pub lists: UserLists,
    /// Players seen before, most recently used first.
    pub user_cache: Vec<tuff_storage::admin::UserCacheEntry>,
    /// Where configuration changes are saved.
    pub config: ConfigHandle,
    /// The config actor, awaited at shutdown so every change is saved.
    pub config_task: tokio::task::JoinHandle<()>,
    /// Told when `/stop` runs.
    pub stop: mpsc::UnboundedSender<()>,
    /// Resolves names nobody here has logged in with, in online mode.
    pub profile_lookup: crate::auth::ProfileLookup,
}

pub struct Coordinator {
    shared: Arc<Shared>,
    messages: mpsc::Receiver<CoordinatorMessage>,
    players_index: watch::Sender<Arc<crate::world::nearby::PlayerIndex>>,
    /// Some player's state changed since the index was last published.
    index_stale: bool,
    waypoints: crate::world::waypoints::WaypointManager,
    /// Something the locator bar depends on changed since it last ran.
    waypoints_stale: bool,
    waypoint_teams: Option<Arc<crate::world::scoreboard::Teams>>,
    /// Non-player entities on locator bars, as the regions last reported.
    entity_transmitters: Vec<crate::world::messages::WaypointTransmitter>,
    /// When the overloaded warning was last logged.
    last_overload_warning: Option<tokio::time::Instant>,
    /// Server ticks in a row with nobody online.
    idle: IdleClock,
    clock: watch::Sender<u64>,
    server_ticks: watch::Sender<u64>,
    clocks: watch::Sender<WorldClocks>,
    tick_state: watch::Sender<TickState>,
    /// The last 100 ticks' own processing time, for `/tick query`.
    tick_samples: Samples,
    /// When the last tick's bookkeeping ran, for tick time samples.
    last_tick_end: Option<std::time::Instant>,
    tick_times: broadcast::Sender<TickTime>,
    tick_metrics: crate::metrics::TickReporter,
    /// Ticks left to run while frozen (`/tick step`).
    step_remaining: u32,
    /// The running `/tick sprint`, if any.
    sprint: Option<Sprint>,
    /// Where the running sprint's report goes when it ends.
    sprint_report: Option<oneshot::Sender<SprintReport>>,
    /// The tick the clock is held at until every barrier member finishes
    /// it, and when it started.
    awaiting_barrier: Option<(u64, tokio::time::Instant)>,
    /// The item ledger at the last save.
    ledger: std::cell::Cell<tuff_item::item::LedgerTotals>,
    spawn: watch::Sender<Spawn>,
    difficulty: watch::Sender<Difficulty>,
    game_rules: watch::Sender<GameRules>,
    weather: watch::Sender<Weather>,
    debug_subscribers: watch::Sender<crate::world::debug_feeds::SharedSubscribers>,
    borders: Vec<watch::Sender<Border>>,
    level: LevelData,
    level_name: String,
    players: BTreeMap<PlayerId, Online>,
    /// Each player's most recent session, so a rejoin waits for its save.
    sessions: HashMap<u128, watch::Receiver<bool>>,
    /// Each real connection's phase channel, by player uuid.
    phases: HashMap<u128, mpsc::UnboundedSender<PhaseRequest>>,
    /// Players who have logged in or been looked up, for commands and the
    /// panel that take names of players who are not online.
    profiles: tuff_storage::admin::UserCache,
    profile_lookup: crate::auth::ProfileLookup,
    status: mpsc::Sender<StatusCommand>,
    properties: ServerProperties,
    property_baseline: std::collections::BTreeMap<String, Option<String>>,
    lists: UserLists,
    config: Option<ConfigHandle>,
    config_task: Option<tokio::task::JoinHandle<()>>,
    stop: mpsc::UnboundedSender<()>,
    rng: StdRng,
    /// Per dimension: players who count for sleeping, and how many sleep,
    /// as last announced.
    sleep_status: HashMap<crate::world::dimension::DimensionId, (usize, usize)>,
    shutdown: Option<oneshot::Sender<()>>,
    lock: Box<dyn WorldLock>,
    /// Feature login gates, owned here so their state needs no lock.
    gates: Vec<Box<dyn crate::world::login_gate::LoginGate>>,
}

impl Coordinator {
    pub fn new(config: CoordinatorConfig) -> Self {
        let gates = config
            .shared
            .login_gates
            .iter()
            .map(|gates| gates.open())
            .collect();
        let coordinator = Self {
            gates,
            players_index: config.players,
            idle: IdleClock::default(),
            index_stale: false,
            waypoints: Default::default(),
            waypoints_stale: false,
            waypoint_teams: None,
            entity_transmitters: Vec::new(),
            last_overload_warning: None,
            shared: config.shared,
            messages: config.messages,
            clock: config.clock,
            server_ticks: config.server_ticks,
            clocks: config.clocks,
            tick_state: config.tick_state,
            tick_samples: Samples::default(),
            last_tick_end: None,
            tick_times: config.tick_times,
            tick_metrics: Default::default(),
            step_remaining: 0,
            sprint: None,
            sprint_report: None,
            awaiting_barrier: None,
            ledger: std::cell::Cell::new(tuff_item::item::conservation::totals()),
            spawn: config.spawn,
            difficulty: config.difficulty,
            game_rules: config.game_rules,
            weather: config.weather,
            debug_subscribers: config.debug_subscribers,
            borders: config.borders,
            level: config.level,
            level_name: config.level_name,
            players: BTreeMap::new(),
            sessions: HashMap::new(),
            phases: HashMap::new(),
            profiles: tuff_storage::admin::UserCache::load(
                config.user_cache,
                tuff_storage::admin::now(),
            ),
            profile_lookup: config.profile_lookup,
            status: config.status,
            properties: config.properties,
            property_baseline: config.property_baseline,
            lists: config.lists,
            config: Some(config.config),
            config_task: Some(config.config_task),
            stop: config.stop,
            rng: StdRng::from_entropy(),
            sleep_status: HashMap::new(),
            shutdown: None,
            lock: config.lock,
        };
        coordinator.sync_gameplay_properties();
        coordinator
    }

    fn sync_gameplay_properties(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        let shared = &self.shared;
        shared
            .spawn_protection
            .store(self.properties.spawn_protection(), Relaxed);
        shared
            .force_game_mode
            .store(self.properties.force_gamemode(), Relaxed);
        shared
            .ops_present
            .store(!self.lists.ops.is_empty(), Relaxed);
    }

    /// Runs the world until shut down, then saves it. The coordinator is
    /// critical: a panic asks the server to stop, sends every player
    /// away and resumes the loop on the same state, so players still
    /// leave (and save) through it before the final save.
    pub async fn run(mut self) {
        let mut critical = Critical::new(
            "the coordinator",
            tuff_supervise::Fatal::channel(self.stop.clone()),
        );
        let mut next_tick = tokio::time::Instant::now();
        loop {
            match CatchUnwind::new(Box::pin(self.serve(&mut next_tick))).await {
                Ok(()) => break,
                Err(panic) if critical.panicked(&*panic) => {
                    for player in self.players.values() {
                        player.handle.send(PlayerEvent::Shutdown);
                    }
                }
                Err(_) => break,
            }
        }
        if let Err(panic) = CatchUnwind::new(Box::pin(self.finish())).await {
            critical.panicked_closing(&*panic);
        }
    }

    /// Drives the tick clock, whose rate and freeze state `/tick`
    /// controls. Messages are always drained (so joins and commands are
    /// handled even while frozen); ticking itself is what pauses.
    async fn serve(&mut self, next_tick: &mut tokio::time::Instant) {
        loop {
            if let Some((tick, started)) = self.awaiting_barrier
                && self.shared.tick_barrier.all_finished(tick)
            {
                self.awaiting_barrier = None;
                self.finish_tick_bookkeeping(started.elapsed());
            }
            if let Some((tick, _)) = self.awaiting_barrier {
                let barrier = self.shared.tick_barrier.clone();
                tokio::select! {
                    message = self.messages.recv() => match message {
                        Some(message) => self.handle(message),
                        None => break,
                    },
                    _ = barrier.progressed_awaiting(tick) => {}
                }
            } else if self.sprint.is_some() {
                match self.messages.try_recv() {
                    Ok(message) => self.handle(message),
                    Err(mpsc::error::TryRecvError::Empty) => {
                        *next_tick = self.start_tick(tokio::time::Instant::now());
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => break,
                }
                tokio::task::yield_now().await;
            } else {
                let frozen = self.tick_state().frozen || self.idle_paused();
                if frozen && self.step_remaining == 0 {
                    tokio::select! {
                        message = self.messages.recv() => match message {
                            Some(message) => self.handle(message),
                            None => break,
                        },
                        _ = tokio::time::sleep_until(*next_tick) => {
                            let started = tokio::time::Instant::now();
                            self.frozen_tick();
                            self.finish_tick_bookkeeping(started.elapsed());
                            *next_tick = next_due(*next_tick, started, self.tick_state().period());
                        }
                    }
                } else {
                    tokio::select! {
                        message = self.messages.recv() => match message {
                            Some(message) => self.handle(message),
                            None => break,
                        },
                        _ = tokio::time::sleep_until(*next_tick) => {
                            *next_tick = self.start_tick(*next_tick);
                        }
                    }
                }
            }
            if self.shutdown.is_some() && self.players.is_empty() {
                break;
            }
        }
    }

    /// Closes every region, saves the level and saved data and releases
    /// the world lock.
    async fn finish(&mut self) {
        let (done, finished) = oneshot::channel();
        if self
            .shared
            .manager
            .send(ManagerMessage::Shutdown { done })
            .await
            .is_ok()
        {
            let _ = finished.await;
        }
        let saved = self.save_level();
        let _ = saved.await;
        self.shared.scoreboard.flush().await;
        self.shared.command_storage.flush().await;
        self.shared.stopwatches.flush().await;
        self.shared.random_sequences.flush().await;
        let shared = self.shared.clone();
        let mut flushing = std::pin::pin!(shared.services.flush());
        loop {
            tokio::select! {
                () = &mut flushing => break,
                Some(message) = self.messages.recv() => self.handle(message),
            }
        }
        self.shared.http.shutdown().await;
        self.shared.web.audit.flush().await;
        self.messages.close();
        self.config = None;
        if let Some(task) = self.config_task.take() {
            let _ = task.await;
        }
        let journal = self.shared.backend.save_journal();
        let lock = std::mem::replace(&mut self.lock, Box::new(Released));
        let _ = tokio::task::spawn_blocking(move || {
            if let Some(journal) = journal {
                journal.finish();
            }
            drop(lock)
        })
        .await;
        log::info!("world saved");
        if let Some(done) = self.shutdown.take() {
            let _ = done.send(());
        }
    }

    /// Saves every player and region as one snapshot: each session
    /// snapshots with no container transaction in flight and takes no new
    /// one until every region has snapshotted, so an item moved between a
    /// player and a container is on exactly one side. The returned
    /// receiver fires once everything is written (with `flush`) or has
    /// been asked to.
    pub(super) fn save_at_barrier(&self, flush: bool) -> oneshot::Receiver<()> {
        let mut snapshots = Vec::new();
        let mut releases = Vec::new();
        let mut written = Vec::new();
        tuff_item::item::conservation::set_checks(self.conservation_checks());
        self.check_item_ledger();
        let barrier = self
            .shared
            .backend
            .save_journal()
            .map(|journal| journal.barrier());
        for player in self.players.values() {
            let (snapshotted, snapshot) = oneshot::channel();
            let (release_tx, release) = oneshot::channel();
            let (done, wrote) = oneshot::channel();
            player
                .handle
                .send(PlayerEvent::Save(crate::world::messages::SaveBarrier {
                    snapshotted,
                    release,
                    written: flush.then_some(done),
                    ticket: barrier.as_ref().map(|barrier| barrier.ticket()),
                }));
            snapshots.push(snapshot);
            releases.push(release_tx);
            written.push(wrote);
        }
        let shared = self.shared.clone();
        let epoch = barrier.as_ref().map(|barrier| barrier.handle());
        let (finished, all) = oneshot::channel();
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + SAVE_BARRIER_WAIT;
            let mut complete = true;
            for snapshot in snapshots {
                complete &= tokio::time::timeout_at(deadline, snapshot).await.is_ok();
            }
            let (saved, regions) = oneshot::channel();
            let (snapshotted, taken) = oneshot::channel();
            let asked = shared
                .manager
                .send(ManagerMessage::SaveAll {
                    done: flush.then_some(saved),
                    snapshotted: Some(snapshotted),
                    epoch,
                })
                .await
                .is_ok();
            complete &=
                asked && matches!(tokio::time::timeout_at(deadline, taken).await, Ok(Ok(())));
            match barrier {
                Some(barrier) if complete => barrier.seal(),
                Some(barrier) => {
                    log::warn!(
                        "the world save did not snapshot within {SAVE_BARRIER_WAIT:?}; \
                         the next save completes it"
                    );
                    barrier.defer();
                }
                None => {}
            }
            drop(releases);
            if flush {
                for wrote in written {
                    let _ = wrote.await;
                }
                if asked {
                    let _ = regions.await;
                }
            }
            let _ = finished.send(());
        });
        all
    }

    /// Saves the level and asks every player and region to save; with
    /// `flush`, `done` waits until all of it and the saved data are
    /// written. Refused while a hold keeps the world's files still.
    fn save_all(&mut self, flush: bool, done: oneshot::Sender<Result<(), Arc<str>>>) {
        let state = self.shared.saves();
        if !state.saves() {
            let holder = state.holder.unwrap_or_else(|| Arc::from("a backup"));
            log::warn!("cannot save: saving is held by {holder}");
            let _ = done.send(Err(holder));
            return;
        }
        self.warn_on_low_disk_space();
        let level = self.save_level();
        let saved = self.save_at_barrier(flush);
        if !flush {
            let _ = done.send(Ok(()));
            return;
        }
        let shared = self.shared.clone();
        tokio::spawn(async move {
            let _ = level.await;
            let _ = saved.await;
            shared.scoreboard.flush().await;
            shared.command_storage.flush().await;
            shared.stopwatches.flush().await;
            shared.random_sequences.flush().await;
            shared.services.flush().await;
            let _ = done.send(Ok(()));
        });
    }

    fn warn_on_low_disk_space(&self) {
        let Some(dir) = crate::panel_host::datapacks::world_dir(&self.shared) else {
            return;
        };
        if !is_low_disk_space(tuff_storage::backup::space::available(&dir)) {
            return;
        }
        log::warn!("Low disk space! Might not be able to save the world.");
        for player in self.players.values().filter(|p| p.entry.is_some()) {
            let admin = self.permission_of(player.profile.id) >= PermissionLevel::Admins;
            if admin {
                player.handle.send(PlayerEvent::LowDiskSpace);
            }
        }
    }

    fn save_level(&mut self) -> tokio::task::JoinHandle<()> {
        self.level.game_time = *self.clock.borrow() as i64;
        self.level.clocks = self.clocks.borrow().clone();
        self.level.spawn = self.spawn.borrow().clone();
        self.level.difficulty = *self.difficulty.borrow();
        self.level.game_rules = self.game_rules.borrow().clone();
        self.level.weather = *self.weather.borrow();
        for (dimension, border) in self.shared.dimensions.iter().zip(&self.borders) {
            if let Some(settings) = self
                .level
                .dimensions
                .iter_mut()
                .find(|settings| settings.key == dimension.key)
            {
                settings.border = border.borrow().saved();
            }
        }
        let custom: Vec<(String, crate::storage::level::WorldBorder)> = self
            .shared
            .dimensions
            .custom()
            .dimensions()
            .iter()
            .filter_map(|resolved| {
                let dimension = self.shared.dimensions.by_key(&resolved.key)?;
                let border = self.borders.get(dimension.id.index())?;
                Some((resolved.key.clone(), border.borrow().saved()))
            })
            .collect();
        self.level.set_data_packs(&self.shared.datapacks.config());
        self.level
            .set_enabled_features(&self.shared.datapacks.features());
        let level = self.level.clone();
        let backend = self.shared.backend.clone();
        let name = self.level_name.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(err) = level.save(backend.documents(), &name, SERVER_BRAND) {
                log::error!("failed to save level data: {err}");
            }
            for (key, border) in custom {
                if let Err(err) = border.write(backend.documents(), &key) {
                    log::error!("failed to save the border of {key}: {err}");
                }
            }
        })
    }

    /// Publishes where every living, non-spectating player is, once a
    /// tick at most and only when something changed.
    fn publish_index(&mut self) {
        if !std::mem::take(&mut self.index_stale) {
            return;
        }
        let players = self.players.values().filter_map(|online| {
            let state = online.state.as_ref()?;
            (state.alive && state.game_mode != crate::protocol::play::GameMode::Spectator).then(
                || {
                    (
                        state.dimension,
                        crate::world::nearby::IndexedPlayer {
                            handle: online.handle.clone(),
                            uuid: online.profile.id,
                            name: Arc::from(online.profile.name.as_str()),
                            position: state.position,
                            looks: state.looks,
                            time_since_rest: state.time_since_rest,
                        },
                    )
                },
            )
        });
        let spectators = self.players.values().filter_map(|online| {
            let state = online.state.as_ref()?;
            (state.alive && state.game_mode == crate::protocol::play::GameMode::Spectator)
                .then_some((state.dimension, state.position))
        });
        let index = crate::world::nearby::PlayerIndex::build(players).with_spectators(spectators);
        self.players_index.send_replace(Arc::new(index));
    }

    fn broadcast_raw(&self, packet: &Bytes) {
        for player in self.players.values().filter(|p| p.entry.is_some()) {
            let _ = player.outbound.send(packet.clone());
        }
    }

    fn broadcast<P: Encode>(&self, packet: &P) {
        self.broadcast_raw(&framed(packet));
    }

    /// Applies a `/worldborder` change and tells the dimension's players.
    fn change_border(
        &mut self,
        dimension: DimensionId,
        change: BorderChange,
    ) -> Result<Option<Resized>, BorderError> {
        let Some(border) = self.borders.get(dimension.index()) else {
            return Err(BorderError::SameSize);
        };
        let mut result = Ok(None);
        border.send_if_modified(|border| {
            result = border.apply(change);
            result.is_ok()
        });
        result?;
        if let Some(packet) = border.borrow().change_packet(change) {
            for player in self.players.values().filter(|p| p.entry.is_some()) {
                player.handle.send(PlayerEvent::Border {
                    dimension,
                    packet: packet.clone(),
                });
            }
        }
        result
    }

    fn publish_status(&self) {
        let online = self
            .players
            .values()
            .filter(|p| p.entry.is_some() && p.fake.is_none())
            .map(|p| OnlinePlayer {
                name: p.profile.name.clone(),
                uuid: p.profile.id,
                listed: p.listed,
            })
            .collect();
        let _ = self.status.try_send(StatusCommand::SetPlayers(online));
    }

    fn online_by_uuid(&self, uuid: u128) -> Option<&Online> {
        self.players
            .values()
            .find(|p| p.entry.is_some() && p.profile.id == uuid)
    }

    fn admin_broadcast(
        &self,
        source: &FeedbackTarget,
        source_name: TextComponent,
        message: TextComponent,
    ) {
        let rules = self.game_rules.borrow();
        let line = TextComponent::translate("chat.type.admin", [source_name, message])
            .color("gray")
            .italic(true);
        if rules.bool("send_command_feedback") {
            for player in self.players.values().filter(|p| p.entry.is_some()) {
                if source.player_id() == Some(player.handle.id) {
                    continue;
                }
                if self.ops_trusted() && self.lists.is_op(player.profile.id) {
                    player.handle.send(PlayerEvent::Message(line.clone()));
                }
            }
        }
        if matches!(
            source,
            FeedbackTarget::Player(_) | FeedbackTarget::CommandBlock(_)
        ) && rules.bool("log_admin_commands")
        {
            log::info!("{}", line.plain());
        }
    }

    fn handle(&mut self, message: CoordinatorMessage) {
        match message {
            CoordinatorMessage::CanLogin {
                user,
                address,
                reply,
            } => {
                let answer = self
                    .refuse_duplicate(&user)
                    .and_then(|()| self.can_login(&user, address))
                    .and_then(|permission| {
                        self.gate_login(&user, address, permission)
                            .map(|()| permission)
                    });
                let _ = reply.send(answer);
            }
            CoordinatorMessage::SpawnFake { owner, spec, reply } => {
                let _ = reply.send(self.spawn_fake(owner, spec));
            }
            CoordinatorMessage::RemoveFake { owner, uuid, reply } => {
                let _ = reply.send(self.remove_fakes(&owner, uuid));
            }
            CoordinatorMessage::Join {
                profile,
                info,
                brand,
                address,
                outbound,
                phase,
                reply,
            } => {
                self.phases.retain(|_, phase| !phase.is_closed());
                self.phases.insert(profile.id, phase);
                let _ = reply.send(self.join(profile, info, brand, address, outbound));
            }
            CoordinatorMessage::Phase { player, request } => {
                if let Some(phase) = self.phases.get(&player) {
                    let _ = phase.send(request);
                }
            }
            CoordinatorMessage::Spawned {
                player,
                entry,
                listed,
                written,
            } => {
                if let (Some(filter), Some(online)) =
                    (self.shared.text_filter.clone(), self.players.get(&player))
                {
                    let profile = online.profile.clone();
                    tokio::spawn(async move { filter.player_joined(&profile).await });
                }
                self.spawned(player, entry, listed);
                let _ = written.send(());
            }
            CoordinatorMessage::Info {
                player,
                actions,
                entry,
                listed,
            } => {
                if let Some(online) = self.players.get_mut(&player)
                    && online.entry.is_some()
                {
                    online.entry = Some(entry.clone());
                    online.listed = listed;
                    if actions == play::player_info::UPDATE_LATENCY {
                        online.latency_dirty = true;
                        return;
                    }
                    self.broadcast(&play::PlayerInfoUpdate {
                        actions,
                        entries: &[entry],
                    });
                    self.publish_status();
                }
            }
            CoordinatorMessage::SetServerIcon { png, reply } => {
                let favicon = match &png {
                    Some(bytes) => match crate::status::Favicon::from_png(bytes) {
                        Ok(favicon) => Some(favicon),
                        Err(err) => {
                            let _ = reply.send(Err(format!("{err:#}")));
                            return;
                        }
                    },
                    None => None,
                };
                let _ = self.status.try_send(StatusCommand::SetFavicon(favicon));
                self.save_config(ConfigRequest::Icon(png));
                let _ = reply.send(Ok(()));
            }
            CoordinatorMessage::GlobalLevelEvent { event, pos, data } => {
                let center = pos.center();
                for online in self.players.values() {
                    let Some(state) = online.state.as_ref().filter(|_| online.entry.is_some())
                    else {
                        continue;
                    };
                    let from = state.position;
                    let delta = [
                        center[0] - from[0],
                        center[1] - from[1],
                        center[2] - from[2],
                    ];
                    let distance =
                        (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt();
                    let at = if distance > 32.0 {
                        let scale = 32.0 / distance;
                        tuff_world_model::BlockPos::new(
                            (from[0] + delta[0] * scale).floor() as i32,
                            (from[1] + delta[1] * scale).floor() as i32,
                            (from[2] + delta[2] * scale).floor() as i32,
                        )
                    } else {
                        pos
                    };
                    let _ = online.outbound.send(framed(&play::LevelEvent {
                        event,
                        position: at,
                        data,
                        global: true,
                    }));
                }
            }
            CoordinatorMessage::Chat {
                player,
                message,
                audience,
            } => {
                let Some(online) = self.players.get(&player) else {
                    return;
                };
                let (name, uuid) = (online.profile.name.clone(), online.profile.id);
                log::info!("<{name}> {message}");
                let chat_type = registry_id(
                    &tuff_vanilla_data::data_registry::CHAT_TYPE,
                    "minecraft:chat",
                );
                let sender = self.shared.permissions.view().decorate_name(
                    uuid,
                    self.shared.scoreboard.teams().player_name(&name, uuid),
                );
                let packet_for = |text: String| {
                    framed(&play::DisguisedChat {
                        message: &TextComponent::new(text),
                        chat_type,
                        sender: &sender,
                        target: None,
                    })
                };
                let own = packet_for(message.clone());
                let others = match audience {
                    ChatAudience::Everyone => Some(own.clone()),
                    ChatAudience::Masked(text) => Some(packet_for(text)),
                    ChatAudience::SenderOnly => None,
                };
                for (id, online) in self.players.iter().filter(|(_, p)| shows_player_chat(p)) {
                    let packet = if *id == player {
                        Some(&own)
                    } else {
                        others.as_ref()
                    };
                    if let Some(packet) = packet {
                        let _ = online.outbound.send(packet.clone());
                    }
                }
            }
            CoordinatorMessage::ChatBroadcast {
                chat_type,
                sender,
                message,
            } => {
                log::info!("[{}] {}", sender.plain(), message.plain());
                let packet = framed(&play::DisguisedChat {
                    message: &message,
                    chat_type: registry_id(&tuff_vanilla_data::data_registry::CHAT_TYPE, chat_type),
                    sender: &sender,
                    target: None,
                });
                for online in self.players.values().filter(|p| shows_player_chat(p)) {
                    let _ = online.outbound.send(packet.clone());
                }
            }
            CoordinatorMessage::Announce(message) => {
                let packet = framed(&play::SystemChat {
                    content: &message,
                    overlay: false,
                });
                for online in self.players.values().filter(|p| shows_system_chat(p)) {
                    let _ = online.outbound.send(packet.clone());
                }
            }
            CoordinatorMessage::Tell { uuid, message } => {
                if let Some(online) = self.online_by_uuid(uuid) {
                    online.handle.send(PlayerEvent::Message(message));
                }
            }
            CoordinatorMessage::PluginInventory { uuid, ops } => {
                let unsent = match self.online_by_uuid(uuid) {
                    Some(online) => match online.handle.try_send(PlayerEvent::PluginInventory(ops))
                    {
                        Ok(()) => Vec::new(),
                        Err(event) => match *event {
                            PlayerEvent::PluginInventory(ops) => ops,
                            _ => Vec::new(),
                        },
                    },
                    None => ops,
                };
                for op in unsent {
                    op.discard();
                }
            }
            CoordinatorMessage::EditInventory { uuid, edit, reply } => {
                self.edit_inventory(uuid, edit, reply);
            }
            CoordinatorMessage::PluginAdvancement {
                uuid,
                grant,
                advancement,
                criterion,
            } => {
                let tree = self.shared.advancements();
                let found = tree.id(&advancement).filter(|id| {
                    criterion
                        .as_deref()
                        .is_none_or(|c| tree.has_criterion(*id, c))
                });
                if let (Some(online), Some(id)) = (self.online_by_uuid(uuid), found) {
                    let (reply, _) = tokio::sync::mpsc::unbounded_channel();
                    online.handle.send(PlayerEvent::Directive {
                        directive: Directive::Advancement {
                            grant,
                            tree,
                            ids: vec![id],
                            criterion,
                            show: grant,
                        },
                        source: DirectiveSource {
                            name: TextComponent::new("Plugin"),
                            player: None,
                        },
                        reply,
                    });
                }
            }
            CoordinatorMessage::PluginTitle { uuid, action } => {
                if let Some(online) = self.online_by_uuid(uuid) {
                    let (reply, _) = tokio::sync::mpsc::unbounded_channel();
                    online.handle.send(PlayerEvent::Directive {
                        directive: Directive::Title(action),
                        source: DirectiveSource {
                            name: TextComponent::new("Plugin"),
                            player: None,
                        },
                        reply,
                    });
                }
            }
            CoordinatorMessage::PluginDirective { uuid, directive } => {
                if let Some(online) = self.online_by_uuid(uuid) {
                    let (reply, _) = tokio::sync::mpsc::unbounded_channel();
                    online.handle.send(PlayerEvent::Directive {
                        directive: *directive,
                        source: DirectiveSource {
                            name: TextComponent::new("Plugin"),
                            player: None,
                        },
                        reply,
                    });
                }
            }
            CoordinatorMessage::TeleportPlayer { uuid, teleport } => {
                if let Some(online) = self.online_by_uuid(uuid) {
                    online.handle.send(PlayerEvent::PluginTeleport(teleport));
                }
            }
            CoordinatorMessage::WaypointTransmitters(found) => {
                if found != self.entity_transmitters {
                    self.entity_transmitters = found;
                    self.waypoints_stale = true;
                }
            }
            CoordinatorMessage::AnnounceDeath { victim, message } => {
                let teams = self.shared.scoreboard.teams();
                let audience = teams.death_audience(&victim);
                let packet = framed(&play::SystemChat {
                    content: &message,
                    overlay: false,
                });
                for player in self.players.values().filter(|p| p.entry.is_some()) {
                    if audience.hears(&player.profile.name) {
                        let _ = player.outbound.send(packet.clone());
                    }
                }
            }
            CoordinatorMessage::PetDied {
                owner,
                victim,
                kind,
                killer,
                killer_player,
            } => {
                let killer = killer_player
                    .and_then(|uuid| self.online_by_uuid(uuid))
                    .map(|p| TextComponent::new(p.profile.name.clone()))
                    .or(killer);
                let message = crate::world::death::death_message(kind, victim, killer, None, None);
                if let Some(online) = self.online_by_uuid(owner) {
                    online.handle.send(PlayerEvent::Message(message));
                }
            }
            CoordinatorMessage::OperatorAlert { line, node, muted } => {
                self.shared.metrics.core().anticheat_alerts.inc();
                self.shared.web.events.publish_alert(&line.plain());
                let view = self.shared.permissions.view();
                for player in self.players.values().filter(|p| p.entry.is_some()) {
                    let subject = crate::permission::Subject::Player {
                        uuid: player.profile.id,
                        level: player.permission,
                    };
                    if !muted.contains(&player.profile.id) && view.allows(subject, node) {
                        player.handle.send(PlayerEvent::Message(line.clone()));
                    }
                }
            }
            CoordinatorMessage::AdminBroadcast {
                source,
                source_name,
                message,
            } => self.admin_broadcast(&source, source_name, message),
            CoordinatorMessage::Clock {
                clock,
                action,
                reply,
            } => {
                let _ = reply.send(self.run_clock(clock, &action));
            }
            CoordinatorMessage::Tick {
                action,
                sprint_report,
                reply,
            } => {
                let _ = reply.send(self.run_tick_command(action, sprint_report));
            }
            CoordinatorMessage::DebugSubscriptions { player, mask } => {
                if let Some(online) = self.players.get_mut(&player) {
                    online.debug = mask;
                }
            }
            CoordinatorMessage::Left { player } => {
                self.index_stale = true;
                let Some(online) = self.players.remove(&player) else {
                    return;
                };
                self.waypoints_stale = true;
                self.shared
                    .scoreboard
                    .set_nametags(vec![(online.profile.name.clone(), None)]);
                if let Some(filter) = self.shared.text_filter.clone() {
                    let profile = online.profile.clone();
                    tokio::spawn(async move { filter.player_left(&profile).await });
                }
                for gate in &mut self.gates {
                    gate.left(
                        &NameAndId {
                            uuid: online.profile.id,
                            name: online.profile.name.clone(),
                        },
                        online.address,
                        std::time::Instant::now(),
                    );
                }
                if online.entry.is_some() {
                    self.broadcast(&play::PlayerInfoRemove(&[online.profile.id]));
                    let name = self
                        .shared
                        .scoreboard
                        .teams()
                        .player_name(&online.profile.name, online.profile.id);
                    self.broadcast(&play::SystemChat {
                        content: &TextComponent::translate("multiplayer.player.left", [name])
                            .color("yellow"),
                        overlay: false,
                    });
                    self.publish_status();
                }
            }
            CoordinatorMessage::PlayerState { player, state } => {
                if let Some(online) = self.players.get_mut(&player) {
                    online.state = Some(state);
                    self.index_stale = true;
                    self.waypoints_stale = true;
                }
            }
            CoordinatorMessage::RoleNames(affected) => self.refresh_role_names(&affected),
            CoordinatorMessage::Players { reply } => {
                let _ = reply.send(self.snapshots());
            }
            CoordinatorMessage::Roster { reply } => {
                let _ = reply.send(self.roster());
            }
            CoordinatorMessage::ProfileOf { player, reply } => {
                let _ = reply.send(self.players.get(&player).map(|p| p.profile.clone()));
            }
            CoordinatorMessage::PermissionOf { uuid, reply } => {
                let _ = reply.send(self.permission_of(uuid));
            }
            CoordinatorMessage::LookupProfile { name, reply } => {
                if let Some(known) = self.profiles.get(&name, tuff_storage::admin::now()) {
                    let _ = reply.send(Some(known));
                } else if self.shared.online_mode {
                    self.look_up_profile(name, reply);
                } else {
                    let _ = reply.send(None);
                }
            }
            CoordinatorMessage::FetchProfile { query, reply } => {
                self.fetch_profile(query, reply);
            }
            CoordinatorMessage::ProfileFound(user) => {
                let now = tuff_storage::admin::now();
                if !self.profiles.contains(&user.name, now) {
                    self.profiles.add(user, now);
                    self.save_user_cache();
                }
            }
            CoordinatorMessage::Sync { reply } => match &self.config {
                Some(config) => {
                    let _ = config.send(ConfigRequest::Sync { reply });
                }
                None => {
                    let _ = reply.send(());
                }
            },
            CoordinatorMessage::ConfigSynced { reply } => {
                let _ = reply.send(());
            }
            CoordinatorMessage::WhitelistReloaded { whitelist, reply } => {
                self.whitelist_reloaded(whitelist, reply);
            }
            CoordinatorMessage::GameRule { name, value, reply } => {
                let _ = reply.send(self.game_rule(&name, value));
            }
            CoordinatorMessage::ServerProperties { change, reply } => {
                if let Some((key, value)) = change {
                    self.set_property(key, value);
                }
                let _ = reply.send(self.properties.entries());
            }
            CoordinatorMessage::Weather {
                kind,
                duration,
                done,
            } => {
                self.weather.send_modify(|weather| {
                    weather.set(kind, duration, &mut self.rng);
                });
                let _ = done.send(());
            }
            CoordinatorMessage::WorldBorder {
                dimension,
                change,
                reply,
            } => {
                let _ = reply.send(self.change_border(dimension, change));
            }
            CoordinatorMessage::Difficulty { difficulty, reply } => {
                let _ = reply.send(self.set_difficulty(difficulty));
            }
            CoordinatorMessage::LockDifficulty { locked } => self.lock_difficulty(locked),
            CoordinatorMessage::DefaultGameMode { mode, reply } => {
                self.level.set_default_game_mode(mode);
                self.shared.live.set_default_game_mode(mode);
                let _ = reply.send(());
            }
            CoordinatorMessage::SetWorldSpawn { spawn, done } => {
                self.spawn.send_replace(spawn.clone());
                self.broadcast(&play::SetDefaultSpawnPosition {
                    dimension: &spawn.dimension,
                    location: spawn.pos,
                    yaw: spawn.yaw,
                    pitch: spawn.pitch,
                });
                let _ = done.send(());
            }
            CoordinatorMessage::Lists { source, op, reply } => self.lists(source, op, reply),
            CoordinatorMessage::SaveAll { flush, done } => self.save_all(flush, done),
            CoordinatorMessage::ChunkIoFailed => self.warn_on_low_disk_space(),
            CoordinatorMessage::Stop => {
                let _ = self.stop.send(());
            }
            #[cfg(feature = "test-hooks")]
            CoordinatorMessage::Panic(message) => panic!("{message}"),
            CoordinatorMessage::Shutdown { done } => {
                self.shutdown = Some(done);
                for player in self.players.values() {
                    player.handle.send(PlayerEvent::Shutdown);
                }
            }
        }
    }

    /// Asks the profile API on its own task, so the coordinator never
    /// waits on the network; a found profile comes back as `ProfileFound`.
    fn look_up_profile(&self, name: String, reply: oneshot::Sender<Option<NameAndId>>) {
        let lookup = self.profile_lookup.clone();
        let coordinator = self.shared.coordinator.clone();
        tokio::spawn(async move {
            let found = match lookup.by_names(std::slice::from_ref(&name)).await {
                Ok(found) => found
                    .into_iter()
                    .find(|(found, _)| found.eq_ignore_ascii_case(&name))
                    .map(|(name, uuid)| NameAndId { uuid, name }),
                Err(error) => {
                    log::warn!("couldn't look up profile {name}: {error:#}");
                    None
                }
            };
            if let Some(user) = &found {
                let _ = coordinator
                    .send(CoordinatorMessage::ProfileFound(user.clone()))
                    .await;
            }
            let _ = reply.send(found);
        });
    }

    /// Answers from the online players when it can; otherwise asks the
    /// profile API on its own task.
    fn fetch_profile(
        &self,
        query: ProfileQuery,
        reply: oneshot::Sender<Option<crate::auth::GameProfile>>,
    ) {
        let online = self.players.values().find(|p| match &query {
            ProfileQuery::Name(name) => p.profile.name.eq_ignore_ascii_case(name),
            ProfileQuery::Id(id) => p.profile.id == *id,
        });
        if let Some(player) = online {
            let _ = reply.send(Some(player.profile.clone()));
            return;
        }
        if !self.shared.online_mode {
            let profile = match query {
                ProfileQuery::Name(name) => Some(crate::auth::GameProfile {
                    id: crate::auth::offline_uuid(&name),
                    name,
                    properties: Vec::new(),
                }),
                ProfileQuery::Id(_) => None,
            };
            let _ = reply.send(profile);
            return;
        }
        let lookup = self.profile_lookup.clone();
        tokio::spawn(async move {
            let id = match query {
                ProfileQuery::Id(id) => Some(id),
                ProfileQuery::Name(name) => {
                    match lookup.by_names(std::slice::from_ref(&name)).await {
                        Ok(found) => found
                            .into_iter()
                            .find(|(found, _)| found.eq_ignore_ascii_case(&name))
                            .map(|(_, uuid)| uuid),
                        Err(error) => {
                            log::warn!("couldn't look up profile {name}: {error:#}");
                            None
                        }
                    }
                }
            };
            let profile = match id {
                Some(id) => lookup.profile_by_id(id).await.unwrap_or_else(|error| {
                    log::warn!("couldn't fetch profile {id:032x}: {error:#}");
                    None
                }),
                None => None,
            };
            let _ = reply.send(profile);
        });
    }

    fn roster(&self) -> Roster {
        let online = self
            .players
            .values()
            .filter_map(|p| {
                let entry = p.entry.as_ref()?;
                Some(RosterEntry {
                    uuid: p.profile.id,
                    name: p.profile.name.clone(),
                    address: p.address,
                    permission: p.permission,
                    latency: entry.latency,
                    brand: p.brand.clone(),
                    game_mode: p.state.as_ref().map_or(entry.game_mode, |s| s.game_mode),
                    dimension: p.state.as_ref().map(|s| s.dimension),
                    position: p.state.as_ref().map(|s| s.position),
                })
            })
            .collect();
        let mut known: Vec<NameAndId> = self
            .profiles
            .users(tuff_storage::admin::now())
            .cloned()
            .collect();
        known.sort_by_key(|a| a.name.to_lowercase());
        Roster {
            online,
            known,
            lists: self.lists.clone(),
            whitelist: self.properties.white_list,
            enforce_whitelist: self.properties.enforce_whitelist,
        }
    }

    fn snapshots(&self) -> Vec<PlayerSnapshot> {
        self.players
            .values()
            .filter(|p| p.entry.is_some())
            .filter_map(|p| {
                Some(PlayerSnapshot {
                    id: p.handle.id,
                    uuid: p.profile.id,
                    name: p.profile.name.clone(),
                    handle: p.handle.clone(),
                    address: p.address,
                    permission: p.permission,
                    state: p.state.clone()?,
                    fake: p.fake.is_some(),
                })
            })
            .collect()
    }
}

fn player_pings(players: &BTreeMap<PlayerId, Online>) -> Vec<crate::metrics::PlayerPing> {
    players
        .values()
        .filter_map(|online| {
            Some(crate::metrics::PlayerPing {
                name: online.profile.name.clone(),
                uuid: crate::resource_pack::format_uuid(online.profile.id),
                ping_ms: online.entry.as_ref()?.latency,
            })
        })
        .collect()
}

/// How many of `active` players must sleep to skip the night.
fn sleepers_needed(active: usize, percentage: i32) -> usize {
    ((active as f32 * percentage as f32 / 100.0).ceil() as usize).max(1)
}

fn text(value: impl ToString) -> TextComponent {
    TextComponent::new(value.to_string())
}

/// Where the world's directory lives within an instance.
pub fn world_dir(instance: &std::path::Path, level_name: &str) -> std::path::PathBuf {
    instance.join(level_name)
}

const LOW_DISK_SPACE_BYTES: u64 = 64 * 1024 * 1024;

fn is_low_disk_space(available: Option<u64>) -> bool {
    available.is_some_and(|free| free < LOW_DISK_SPACE_BYTES)
}

const OVERLOADED_THRESHOLD: std::time::Duration = std::time::Duration::from_secs(1);
const OVERLOADED_TICKS: u32 = 20;
const OVERLOADED_WARNING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);
const OVERLOADED_WARNING_TICKS: u32 = 100;

/// The whole ticks a tick due at `due` and started at `started` is behind,
/// when that is far enough behind to warn and the last warning is old enough.
fn overloaded_by(
    due: tokio::time::Instant,
    started: tokio::time::Instant,
    period: std::time::Duration,
    last_warning: Option<tokio::time::Instant>,
) -> Option<u128> {
    let behind = started.saturating_duration_since(due);
    if period.is_zero() || behind <= OVERLOADED_THRESHOLD + period * OVERLOADED_TICKS {
        return None;
    }
    let quiet = last_warning.is_none_or(|last| {
        due.saturating_duration_since(last)
            >= OVERLOADED_WARNING_INTERVAL + period * OVERLOADED_WARNING_TICKS
    });
    quiet.then(|| behind.as_nanos() / period.as_nanos())
}

/// One period after `due`, so ticks that start late are caught up, unless
/// the clock fell more than [`CATCH_UP_LIMIT`] behind.
fn next_due(
    due: tokio::time::Instant,
    started: tokio::time::Instant,
    period: std::time::Duration,
) -> tokio::time::Instant {
    if started.saturating_duration_since(due) > CATCH_UP_LIMIT {
        started + period
    } else {
        due + period
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod disk_space_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn warns_below_sixty_four_mebibytes_only() {
        assert!(is_low_disk_space(Some(0)));
        assert!(is_low_disk_space(Some(LOW_DISK_SPACE_BYTES - 1)));
        assert!(!is_low_disk_space(Some(LOW_DISK_SPACE_BYTES)));
        assert!(!is_low_disk_space(None));
    }
}

mod access;
mod config;
mod idle;
mod inventory_edits;
mod ledger;
mod tick;
use idle::IdleClock;

#[cfg(test)]
mod idle_tests;
