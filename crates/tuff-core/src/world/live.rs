//! server.properties values that change while the world runs. The
//! coordinator owns the properties and is the only writer; every other
//! actor reads the current value when it needs it.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering::Relaxed};

use tokio::sync::watch;

use super::{DEFAULT_VIEW_DISTANCE, NetworkSettings, SimulationSettings};
use crate::protocol::play::GameMode;

pub const CHUNK_BUDGET_KEY: &str = "tuff.world.chunkBudgetPerPlayer";
pub const DEFAULT_CHUNK_BUDGET: u32 = 64;

pub fn config_keys() -> Vec<crate::config::Key> {
    vec![
        crate::config::Key::new(
            CHUNK_BUDGET_KEY,
            crate::config::Value::Int(i64::from(DEFAULT_CHUNK_BUDGET)),
            "Most chunks one player may have requested from the world but not yet received, nearest first, refilled as chunks arrive so a fast traveller cannot flood generation (0 is unlimited; applies on restart)",
        )
        .validated(|v| match v {
            crate::config::Value::Int(n) if (0..=1_000_000).contains(n) => Ok(()),
            _ => Err("must be 0 or more".to_owned()),
        }),
    ]
}

/// The live part of the server settings, shared by every actor.
pub struct LiveSettings {
    view_distance: AtomicI32,
    simulation_distance: AtomicI32,
    max_players: AtomicU32,
    pause_when_empty_seconds: AtomicI32,
    broadcast_range_percent: AtomicI32,
    max_chained_neighbor_updates: AtomicI32,
    default_game_mode: AtomicI32,
    chunk_budget: AtomicU32,
    network: watch::Sender<Arc<NetworkSettings>>,
}

/// Which live setting a player session must react to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveChange {
    /// Connection policy: idle timeout, rate limit, spam thresholds.
    Connection,
    ViewDistance,
    SimulationDistance,
    /// `entity-broadcast-range-percentage`: which entities are shown.
    BroadcastRange,
    /// The default game mode or whether it is forced.
    GameMode,
}

impl Default for LiveSettings {
    fn default() -> Self {
        Self::new(
            DEFAULT_VIEW_DISTANCE,
            20,
            GameMode::Survival,
            SimulationSettings::default(),
            NetworkSettings::default(),
        )
    }
}

impl LiveSettings {
    pub fn new(
        view_distance: i32,
        max_players: u32,
        default_game_mode: GameMode,
        simulation: SimulationSettings,
        network: NetworkSettings,
    ) -> Self {
        Self {
            view_distance: AtomicI32::new(view_distance),
            simulation_distance: AtomicI32::new(simulation.simulation_distance),
            max_players: AtomicU32::new(max_players),
            pause_when_empty_seconds: AtomicI32::new(simulation.pause_when_empty_seconds),
            broadcast_range_percent: AtomicI32::new(simulation.broadcast_range_percent),
            max_chained_neighbor_updates: AtomicI32::new(
                crate::world::redstone::MAX_CHAINED_UPDATES as i32,
            ),
            default_game_mode: AtomicI32::new(default_game_mode.id()),
            chunk_budget: AtomicU32::new(DEFAULT_CHUNK_BUDGET),
            network: watch::Sender::new(Arc::new(network)),
        }
    }

    /// The farthest a player may see, in chunks.
    pub fn view_distance(&self) -> i32 {
        self.view_distance.load(Relaxed)
    }

    /// Chunks around a player (Chebyshev) that tick.
    pub fn simulation_distance(&self) -> i32 {
        self.simulation_distance.load(Relaxed)
    }

    pub fn max_players(&self) -> u32 {
        self.max_players.load(Relaxed)
    }

    /// Seconds with no players before the game stops ticking; negative
    /// never pauses.
    pub fn pause_when_empty_seconds(&self) -> i32 {
        self.pause_when_empty_seconds.load(Relaxed)
    }

    /// Scales how far away other entities are shown to a player.
    pub fn broadcast_range_percent(&self) -> i32 {
        self.broadcast_range_percent.load(Relaxed)
    }

    /// Neighbour updates one cascade may run before the rest are dropped;
    /// negative is unlimited.
    pub fn max_chained_neighbor_updates(&self) -> i32 {
        self.max_chained_neighbor_updates.load(Relaxed)
    }

    /// The game mode new players start in.
    pub fn default_game_mode(&self) -> GameMode {
        GameMode::from_id(self.default_game_mode.load(Relaxed)).unwrap_or(GameMode::Survival)
    }

    /// The connection policy now; joins in progress keep the one they read.
    pub fn network(&self) -> Arc<NetworkSettings> {
        self.network.borrow().clone()
    }

    /// Most chunks one player may have requested but not yet received (0 is unlimited).
    pub fn chunk_budget(&self) -> u32 {
        self.chunk_budget.load(Relaxed)
    }

    pub fn set_chunk_budget(&self, chunks: u32) {
        self.chunk_budget.store(chunks, Relaxed);
    }

    pub fn set_view_distance(&self, chunks: i32) {
        self.view_distance.store(chunks, Relaxed);
    }

    pub fn set_simulation_distance(&self, chunks: i32) {
        self.simulation_distance.store(chunks, Relaxed);
    }

    pub fn set_max_players(&self, players: u32) {
        self.max_players.store(players, Relaxed);
    }

    pub fn set_pause_when_empty_seconds(&self, seconds: i32) {
        self.pause_when_empty_seconds.store(seconds, Relaxed);
    }

    pub fn set_max_chained_neighbor_updates(&self, limit: i32) {
        self.max_chained_neighbor_updates.store(limit, Relaxed);
    }

    pub fn set_broadcast_range_percent(&self, percent: i32) {
        self.broadcast_range_percent.store(percent, Relaxed);
    }

    pub fn set_default_game_mode(&self, mode: GameMode) {
        self.default_game_mode.store(mode.id(), Relaxed);
    }

    /// Changes the connection policy in one step.
    pub fn update_network(&self, change: impl FnOnce(&mut NetworkSettings)) {
        self.network
            .send_modify(|network| change(Arc::make_mut(network)));
    }
}
