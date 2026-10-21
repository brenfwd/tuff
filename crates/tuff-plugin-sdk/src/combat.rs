// SPDX-License-Identifier: MIT
//! Melee attack and damage filters the server enforces without running
//! guest code. Declare them while registering.
//!
//! [`attack`] cancels a player's melee hit on a non-player entity and is
//! reported as a `player-attack` event; [`damage`] cancels or scales damage
//! to players (by cause, attacker, region) and is reported as a
//! `damage-filtered` event.

use crate::{host, types, uuid_of};

/// An attack filter being built; every set field must match.
#[derive(Default, Clone)]
pub struct AttackRule {
    filter: Option<types::AttackFilter>,
}

fn blank_attack() -> types::AttackFilter {
    types::AttackFilter {
        kind: None,
        tag: None,
        held_item: None,
        attacker: None,
        dimension: None,
        area: None,
    }
}

/// Starts an attack filter that matches every attack.
pub fn attack() -> AttackRule {
    AttackRule {
        filter: Some(blank_attack()),
    }
}

impl AttackRule {
    fn edit(mut self, f: impl FnOnce(&mut types::AttackFilter)) -> Self {
        f(self.filter.get_or_insert_with(blank_attack));
        self
    }

    /// Only victims of this entity type.
    pub fn victim_kind(self, kind: &str) -> Self {
        self.edit(|f| f.kind = Some(kind.to_owned()))
    }

    /// Only victims with this scoreboard tag.
    pub fn victim_tag(self, tag: &str) -> Self {
        self.edit(|f| f.tag = Some(tag.to_owned()))
    }

    /// Only attacks with this item held.
    pub fn held_item(self, item: &str) -> Self {
        self.edit(|f| f.held_item = Some(item.to_owned()))
    }

    /// Only attacks by this player (32 hex digits).
    pub fn attacker(self, uuid: &str) -> Self {
        self.edit(|f| f.attacker = uuid_of(uuid))
    }

    /// Only victims in this dimension.
    pub fn dimension(self, key: &str) -> Self {
        self.edit(|f| f.dimension = Some(key.to_owned()))
    }

    /// Only victims inside the box between two corners.
    pub fn area(self, a: types::Vec3, b: types::Vec3) -> Self {
        self.edit(|f| f.area = Some((a, b)))
    }

    /// Registers the filter: matching attacks are cancelled.
    pub fn cancel(self) -> Result<(), types::RegisterError> {
        host::intercept_attack(&self.filter.unwrap_or_else(blank_attack))
    }
}

/// A damage filter being built; every set field must match.
#[derive(Default, Clone)]
pub struct DamageRule {
    filter: Option<types::DamageFilter>,
}

fn blank_damage() -> types::DamageFilter {
    types::DamageFilter {
        victim: None,
        causes: Vec::new(),
        attacker: None,
        attacker_kind: None,
        dimension: None,
        area: None,
        scale: 0.0,
    }
}

/// Starts a damage filter that matches all damage to players.
pub fn damage() -> DamageRule {
    DamageRule {
        filter: Some(blank_damage()),
    }
}

impl DamageRule {
    fn edit(mut self, f: impl FnOnce(&mut types::DamageFilter)) -> Self {
        f(self.filter.get_or_insert_with(blank_damage));
        self
    }

    /// Only this victim (32 hex digits).
    pub fn victim(self, uuid: &str) -> Self {
        self.edit(|f| f.victim = uuid_of(uuid))
    }

    /// Also matches this damage type (`minecraft:fall`, `minecraft:out_of_world`).
    pub fn cause(self, cause: &str) -> Self {
        self.edit(|f| f.causes.push(cause.to_owned()))
    }

    /// Only damage dealt by this entity (32 hex digits).
    pub fn attacker(self, uuid: &str) -> Self {
        self.edit(|f| f.attacker = uuid_of(uuid))
    }

    /// Only damage dealt by this entity type.
    pub fn attacker_kind(self, kind: &str) -> Self {
        self.edit(|f| f.attacker_kind = Some(kind.to_owned()))
    }

    /// Only victims in this dimension.
    pub fn dimension(self, key: &str) -> Self {
        self.edit(|f| f.dimension = Some(key.to_owned()))
    }

    /// Only victims inside the box between two corners.
    pub fn area(self, a: types::Vec3, b: types::Vec3) -> Self {
        self.edit(|f| f.area = Some((a, b)))
    }

    /// Registers the filter: matching hits do nothing at all.
    pub fn cancel(self) -> Result<(), types::RegisterError> {
        self.scale(0.0)
    }

    /// Registers the filter: matching hits are multiplied by `factor`.
    pub fn scale(self, factor: f32) -> Result<(), types::RegisterError> {
        let mut filter = self.filter.unwrap_or_else(blank_damage);
        filter.scale = factor;
        host::intercept_damage(&filter)
    }
}
