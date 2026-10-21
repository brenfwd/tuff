// SPDX-License-Identifier: MIT
//! Reads and changes of online players addressed by uuid (32 hex digits),
//! usable from timers and other callbacks the player does not act in.

use crate::types::{self, GameMode, Location, Player, PlayerState, Rotation};
use crate::{WorldError, host};

fn uuid_of(uuid: &str) -> Result<types::Uuid, WorldError> {
    (uuid.len() == 32)
        .then(|| types::Uuid::parse(uuid))
        .flatten()
        .ok_or(WorldError::Unavailable)
}

/// Every online player.
pub fn online() -> Vec<Player> {
    host::online_players()
}

/// The player's state, or `None` when they are not online.
pub fn state(uuid: &str) -> Option<PlayerState> {
    host::player_info(uuid_of(uuid).ok()?)
}

/// The player's location, or `None` when they are not online.
pub fn location(uuid: &str) -> Option<Location> {
    host::player_location(uuid_of(uuid).ok()?)
}

pub fn set_game_mode(uuid: &str, mode: GameMode) -> Result<(), WorldError> {
    host::set_game_mode(uuid_of(uuid)?, mode).map_err(WorldError::from)
}

/// Sets health (above zero; use [`kill`] to kill) and food level of a
/// living player; `None` leaves a value as it is.
pub fn set_vitals(uuid: &str, health: Option<f32>, food: Option<i32>) -> Result<(), WorldError> {
    host::set_vitals(uuid_of(uuid)?, health, food).map_err(WorldError::from)
}

/// Sets experience to `level` and `points` into it.
pub fn set_xp(uuid: &str, level: i32, points: i32) -> Result<(), WorldError> {
    host::set_xp(uuid_of(uuid)?, level, points).map_err(WorldError::from)
}

pub fn add_effect(
    uuid: &str,
    effect: &str,
    duration_ticks: i32,
    amplifier: u8,
    particles: bool,
) -> Result<(), WorldError> {
    host::add_effect(uuid_of(uuid)?, effect, duration_ticks, amplifier, particles)
        .map_err(WorldError::from)
}

/// Clears one effect, or all of them when `effect` is `None`.
pub fn clear_effects(uuid: &str, effect: Option<&str>) -> Result<(), WorldError> {
    host::clear_effects(uuid_of(uuid)?, effect).map_err(WorldError::from)
}

pub fn kill(uuid: &str) -> Result<(), WorldError> {
    host::kill_player(uuid_of(uuid)?).map_err(WorldError::from)
}

/// Revives a dead player at their respawn point, skipping the death screen.
pub fn respawn(uuid: &str) -> Result<(), WorldError> {
    host::respawn_player(uuid_of(uuid)?).map_err(WorldError::from)
}

/// Sets the player's forced respawn point.
pub fn set_respawn_point(
    uuid: &str,
    dimension: &str,
    block: [i32; 3],
    yaw: f32,
    pitch: f32,
) -> Result<(), WorldError> {
    let pos = types::BlockPos {
        x: block[0],
        y: block[1],
        z: block[2],
    };
    host::set_respawn_point(uuid_of(uuid)?, dimension, pos, Rotation { yaw, pitch })
        .map_err(WorldError::from)
}
