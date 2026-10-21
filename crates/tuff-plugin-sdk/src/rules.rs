// SPDX-License-Identifier: MIT
//! Rules the server enforces for the plugin without running its code:
//! block rules that cancel breaks and placements and spare blocks from
//! explosions, and teams whose members cannot hurt each other. Also what
//! hurt a player and what a projectile struck, while handling those
//! events.
//!
//! ```ignore
//! use tuff_plugin_sdk::rules::{self, BlockRule};
//!
//! let arena = BlockRule::protect("minecraft:overworld", (0, 60, 0), (99, 120, 99))
//!     .breakable(["red_bed", "blue_bed"])
//!     .blast_proof(["glass", "end_stone"]);
//! let id = rules::add_block_rule(&arena)?;
//! rules::set_team(player, Some("red"))?;
//! ```

use crate::bindings::tuff::plugin::host;
use crate::types::{BlockPos, Uuid};
pub use crate::types::{BlockRule, ChangeRule, DamageSource, Hit};

impl BlockRule {
    /// A box in `dimension` between two corners where players may place
    /// blocks and break, and explosions destroy, only what players placed
    /// after the rule was added.
    pub fn protect(dimension: &str, a: (i32, i32, i32), b: (i32, i32, i32)) -> Self {
        let pos = |(x, y, z)| BlockPos { x, y, z };
        Self {
            dimension: dimension.to_owned(),
            min: pos(a),
            max: pos(b),
            place: true,
            break_placed: true,
            breakable: Vec::new(),
            explode_placed: true,
            blast_proof: Vec::new(),
            cancel_explosions: false,
        }
    }

    /// Cancels every explosion centred inside the box whole.
    pub fn cancel_explosions(mut self) -> Self {
        self.cancel_explosions = true;
        self
    }

    /// Block ids players may break anyway, such as beds.
    pub fn breakable<S: Into<String>>(mut self, ids: impl IntoIterator<Item = S>) -> Self {
        self.breakable.extend(ids.into_iter().map(Into::into));
        self
    }

    /// Block ids explosions never destroy, even placed ones.
    pub fn blast_proof<S: Into<String>>(mut self, ids: impl IntoIterator<Item = S>) -> Self {
        self.blast_proof.extend(ids.into_iter().map(Into::into));
        self
    }

    /// No placing at all inside.
    pub fn no_placing(mut self) -> Self {
        self.place = false;
        self
    }
}

/// Adds `rule` until it is removed or the plugin unloads; its id. Adding
/// is the mark that "placed after the rule" counts from.
pub fn add_block_rule(rule: &BlockRule) -> Result<u32, crate::HostError> {
    host::add_block_rule(rule).map_err(Into::into)
}

impl ChangeRule {
    /// A box in `dimension` between two corners where fire, liquids and
    /// entities change nothing.
    pub fn freeze(dimension: &str, a: (i32, i32, i32), b: (i32, i32, i32)) -> Self {
        let pos = |(x, y, z)| BlockPos { x, y, z };
        Self {
            dimension: dimension.to_owned(),
            min: pos(a),
            max: pos(b),
            fire_spread: false,
            fire_burn: false,
            liquid_flow: false,
            entity_grief: false,
        }
    }
}

/// Adds `rule` until it is removed or the plugin unloads; its id, which
/// `remove_block_rule` takes too.
pub fn add_change_rule(rule: &ChangeRule) -> Result<u32, crate::HostError> {
    host::add_change_rule(rule).map_err(Into::into)
}

/// False when `id` is not one of this plugin's rules.
pub fn remove_block_rule(id: u32) -> bool {
    host::remove_block_rule(id)
}

/// Puts `player` in this plugin's team `team`, or out of every team.
pub fn set_team(player: Uuid, team: Option<&str>) -> Result<(), crate::HostError> {
    host::set_team(player, team).map_err(Into::into)
}

/// What hurt the player, while handling a player-hurt or player-death
/// event.
pub fn acting_damage() -> Option<DamageSource> {
    host::acting_damage()
}

/// What the projectile struck, while handling a projectile-hit event.
pub fn acting_hit() -> Option<Hit> {
    host::acting_hit()
}
