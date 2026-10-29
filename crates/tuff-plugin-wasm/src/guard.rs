//! Rules plugins set that the server enforces without running guest code:
//! block rules (cancelled breaks and placements, explosions sparing
//! blocks), plugin teams (no friendly fire), plus the events that come
//! with them: uses of an item in the air, projectile hits and who to
//! credit for damage.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tuff_core::protocol::play::{self, Serverbound};
use tuff_core::world::entity::{Culprit, ProjectileLanding};
use tuff_core::world::explosion::ExplosionCause;
use tuff_core::world::inventory::Inventory;
use tuff_core::world::region_api::{
    BlockAction, BlockCause, PlayerBlockAction, RegionHooks, RegionWorld,
};
use tuff_core::world::session::hooks::{PacketInterceptor, SessionHooks, Verdict};
use tuff_core::world::session::{DamageSource, HurtBy, HurtResult, PlayerSession};
use tuff_vanilla_data::Block;
use tuff_vanilla_data::block::BlockState;
use tuff_vanilla_data::registry::EntityType;
use tuff_world_model::BlockPos;

use crate::host::WasmHost;
use crate::sandbox::{Player, uuid};
use crate::wit::bindings::types::{
    self as wit, BlockRule, ChangeRule, ClickKind, Damage, DamageSource as WitDamage, Death,
    Entity, Event, EventKinds, FakeDamage, Hit, HostError, ItemUse, Rotation, Vec3,
};

/// Block rules one plugin may have at once.
const MAX_GUARD_OPS: usize = crate::abi::MAX_GUARD_OPS;
const MAX_RULES: usize = 64;
/// Block ids in one rule's `breakable` or `blast-proof` list.
pub const MAX_RULE_BLOCKS: usize = 64;
/// Players one plugin may put in teams.
pub const MAX_TEAM_MEMBERS: usize = 1024;
/// Bytes of a team name.
pub const MAX_TEAM_NAME: usize = 64;
/// Ticks a hit by a player or mob still earns the kill.
pub const CREDIT_TICKS: u64 = 100;

/// A block rule as the server checks it.
#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub dimension: String,
    pub min: BlockPos,
    pub max: BlockPos,
    pub place: bool,
    pub break_placed: bool,
    pub breakable: Vec<String>,
    pub explode_placed: bool,
    pub blast_proof: Vec<String>,
    pub cancel_explosions: bool,
}

impl Rule {
    pub fn parse(rule: BlockRule) -> Result<Self, String> {
        let blocks = |ids: Vec<String>| -> Result<Vec<String>, String> {
            if ids.len() > MAX_RULE_BLOCKS {
                return Err(format!("at most {MAX_RULE_BLOCKS} block ids per list"));
            }
            ids.into_iter()
                .map(|id| {
                    let name = crate::portal::qualified(&id);
                    match crate::world::parse_state(&name) {
                        Some(_) => Ok(name),
                        None => Err(format!("{id} is not a block")),
                    }
                })
                .collect()
        };
        let (a, b) = (rule.min, rule.max);
        Ok(Self {
            dimension: crate::portal::qualified(&rule.dimension),
            min: BlockPos::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)),
            max: BlockPos::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)),
            place: rule.place,
            break_placed: rule.break_placed,
            breakable: blocks(rule.breakable)?,
            explode_placed: rule.explode_placed,
            blast_proof: blocks(rule.blast_proof)?,
            cancel_explosions: rule.cancel_explosions,
        })
    }

    fn covers(&self, dimension: &str, pos: BlockPos) -> bool {
        (self.min.x..=self.max.x).contains(&pos.x)
            && (self.min.y..=self.max.y).contains(&pos.y)
            && (self.min.z..=self.max.z).contains(&pos.z)
            && self.dimension == dimension
    }
}

fn listed(list: &[String], block: &str) -> bool {
    list.iter().any(|id| crate::portal::same_block(id, block))
}

/// A box where changes no player makes are stopped, as the server checks it.
#[derive(Clone, Debug, PartialEq)]
pub struct ChangeBox {
    pub dimension: String,
    pub min: BlockPos,
    pub max: BlockPos,
    pub fire_spread: bool,
    pub fire_burn: bool,
    pub liquid_flow: bool,
    pub entity_grief: bool,
}

impl ChangeBox {
    pub fn parse(rule: ChangeRule) -> Self {
        let (a, b) = (rule.min, rule.max);
        Self {
            dimension: crate::portal::qualified(&rule.dimension),
            min: BlockPos::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)),
            max: BlockPos::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)),
            fire_spread: rule.fire_spread,
            fire_burn: rule.fire_burn,
            liquid_flow: rule.liquid_flow,
            entity_grief: rule.entity_grief,
        }
    }

    fn covers(&self, dimension: &str, pos: BlockPos) -> bool {
        (self.min.x..=self.max.x).contains(&pos.x)
            && (self.min.y..=self.max.y).contains(&pos.y)
            && (self.min.z..=self.max.z).contains(&pos.z)
            && self.dimension == dimension
    }

    /// Whether this box lets `cause` turn `old` into `new`.
    pub fn allows(&self, old: Block, new: Block, cause: BlockCause) -> bool {
        let is_fire = |block| matches!(block, Block::Fire | Block::SoulFire);
        match cause {
            BlockCause::Block(source) if is_fire(source) => {
                if is_fire(new) && !is_fire(old) {
                    self.fire_spread
                } else if !is_fire(old) && !is_fire(new) {
                    self.fire_burn
                } else {
                    true
                }
            }
            BlockCause::Block(Block::Water | Block::Lava) => {
                self.liquid_flow || !matches!(new, Block::Water | Block::Lava)
            }
            BlockCause::Entity { .. } => self.entity_grief,
            _ => true,
        }
    }
}

/// A change a call asked for, carried out once it returns.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    AddRule(u32, Rule),
    AddChange(u32, ChangeBox),
    RemoveRule(u32),
    Team(u128, Option<String>),
}

/// What an event comes with for the `acting-*` imports.
#[derive(Clone, Debug, PartialEq)]
pub enum Detail {
    Damage(WitDamage),
    Hit(Hit),
}

/// What the sandbox keeps between calls to answer the guard imports.
#[derive(Debug, Default)]
pub struct Calls {
    next_rule: u32,
    rules: BTreeSet<u32>,
    changes: BTreeSet<u32>,
    teams: HashMap<u128, String>,
    /// What the event being handled comes with.
    pub detail: Option<Detail>,
}

impl Calls {
    pub fn add_rule(&mut self, ops: &mut Vec<Op>, rule: BlockRule) -> Result<u32, HostError> {
        if self.rules.len() + self.changes.len() >= MAX_RULES || ops.len() >= MAX_GUARD_OPS {
            return Err(HostError::LimitReached);
        }
        let rule = Rule::parse(rule).map_err(HostError::Invalid)?;
        self.next_rule = self.next_rule.wrapping_add(1);
        let id = self.next_rule;
        self.rules.insert(id);
        ops.push(Op::AddRule(id, rule));
        Ok(id)
    }

    pub fn add_change(&mut self, ops: &mut Vec<Op>, rule: ChangeRule) -> Result<u32, HostError> {
        if self.rules.len() + self.changes.len() >= MAX_RULES || ops.len() >= MAX_GUARD_OPS {
            return Err(HostError::LimitReached);
        }
        self.next_rule = self.next_rule.wrapping_add(1);
        let id = self.next_rule;
        self.changes.insert(id);
        ops.push(Op::AddChange(id, ChangeBox::parse(rule)));
        Ok(id)
    }

    pub fn remove_rule(&mut self, ops: &mut Vec<Op>, id: u32) -> bool {
        if ops.len() >= MAX_GUARD_OPS {
            return false;
        }
        let known = self.rules.remove(&id) | self.changes.remove(&id);
        if known {
            ops.push(Op::RemoveRule(id));
        }
        known
    }

    pub fn set_team(
        &mut self,
        ops: &mut Vec<Op>,
        player: u128,
        team: Option<String>,
    ) -> Result<(), HostError> {
        if ops.len() >= MAX_GUARD_OPS {
            return Err(HostError::LimitReached);
        }
        match &team {
            Some(name) if name.is_empty() || name.len() > MAX_TEAM_NAME => {
                return Err(HostError::Invalid(format!(
                    "a team name is 1 to {MAX_TEAM_NAME} bytes"
                )));
            }
            Some(name) => {
                if !self.teams.contains_key(&player) && self.teams.len() >= MAX_TEAM_MEMBERS {
                    return Err(HostError::LimitReached);
                }
                self.teams.insert(player, name.clone());
            }
            None => {
                self.teams.remove(&player);
            }
        }
        ops.push(Op::Team(player, team));
        Ok(())
    }
}

/// A rule in force, with the serial it was added under.
#[derive(Clone, Debug, PartialEq)]
pub struct Active {
    pub serial: u64,
    pub rule: Rule,
}

/// One plugin's rules and teams, as the host keeps them.
#[derive(Debug, Default)]
pub struct Owned {
    rules: BTreeMap<u32, Active>,
    changes: BTreeMap<u32, ChangeBox>,
    teams: HashMap<u128, String>,
}

impl Owned {
    /// Carries out `ops`, numbering new rules from `serial`; true if
    /// anything changed.
    pub fn apply(&mut self, ops: Vec<Op>, serial: &mut u64) -> bool {
        let changed = !ops.is_empty();
        for op in ops {
            match op {
                Op::AddRule(id, rule) => {
                    *serial += 1;
                    self.rules.insert(
                        id,
                        Active {
                            serial: *serial,
                            rule,
                        },
                    );
                }
                Op::AddChange(id, rule) => {
                    self.changes.insert(id, rule);
                }
                Op::RemoveRule(id) => {
                    self.rules.remove(&id);
                    self.changes.remove(&id);
                }
                Op::Team(player, Some(team)) => {
                    self.teams.insert(player, team);
                }
                Op::Team(player, None) => {
                    self.teams.remove(&player);
                }
            }
        }
        changed
    }
}

/// Every loaded plugin's rules and teams, for sessions and regions.
#[derive(Debug, Default)]
pub struct Published {
    /// The latest rule serial: a block placed now counts as placed after
    /// every rule with this serial or lower.
    pub serial: u64,
    pub rules: Vec<Active>,
    pub changes: Vec<ChangeBox>,
    pub teams: Vec<HashMap<u128, String>>,
}

impl Published {
    pub fn build<'a>(owned: impl Iterator<Item = &'a Owned>, serial: u64) -> Self {
        let mut published = Self {
            serial,
            ..Self::default()
        };
        for owned in owned {
            published.rules.extend(owned.rules.values().cloned());
            published.changes.extend(owned.changes.values().cloned());
            if !owned.teams.is_empty() {
                published.teams.push(owned.teams.clone());
            }
        }
        published
    }

    /// Whether some plugin has `a` and `b` in the same team.
    pub fn teammates(&self, a: u128, b: u128) -> bool {
        self.teams
            .iter()
            .any(|teams| match (teams.get(&a), teams.get(&b)) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            })
    }

    fn covering<'a>(
        &'a self,
        dimension: &'a str,
        pos: BlockPos,
    ) -> impl Iterator<Item = &'a Active> {
        self.rules
            .iter()
            .filter(move |active| active.rule.covers(dimension, pos))
    }

    /// Whether every change box covering `pos` lets `cause` turn `old`
    /// into `new`.
    pub fn may_change(
        &self,
        dimension: &str,
        pos: BlockPos,
        old: Block,
        new: Block,
        cause: BlockCause,
    ) -> bool {
        self.changes
            .iter()
            .filter(|rule| rule.covers(dimension, pos))
            .all(|rule| rule.allows(old, new, cause))
    }

    /// Whether a placement at `pos` should be remembered.
    pub fn tracks(&self, dimension: &str, pos: BlockPos) -> bool {
        self.covering(dimension, pos).next().is_some()
    }

    pub fn may_place(&self, dimension: &str, pos: BlockPos) -> bool {
        self.covering(dimension, pos)
            .all(|active| active.rule.place)
    }

    /// Whether a player may break `block` at `pos`, placed by a player
    /// under serial `placed` if at all.
    pub fn may_break(
        &self,
        dimension: &str,
        pos: BlockPos,
        block: &str,
        placed: Option<u64>,
    ) -> bool {
        self.covering(dimension, pos).all(|active| {
            listed(&active.rule.breakable, block)
                || (active.rule.break_placed && placed.is_some_and(|at| at >= active.serial))
        })
    }

    /// Whether an explosion centred at `center` goes off at all.
    pub fn may_detonate(&self, dimension: &str, center: [f64; 3]) -> bool {
        let pos = BlockPos::new(
            center[0].floor() as i32,
            center[1].floor() as i32,
            center[2].floor() as i32,
        );
        self.covering(dimension, pos)
            .all(|active| !active.rule.cancel_explosions)
    }

    pub fn may_explode(
        &self,
        dimension: &str,
        pos: BlockPos,
        block: &str,
        placed: Option<u64>,
    ) -> bool {
        self.covering(dimension, pos).all(|active| {
            active.rule.explode_placed
                && !listed(&active.rule.blast_proof, block)
                && placed.is_some_and(|at| at >= active.serial)
        })
    }
}

/// Blocks players placed in this region under a rule, with the serial
/// in force then.
#[derive(Default)]
struct Placed(HashMap<BlockPos, u64>);

fn published(world: &dyn RegionWorld) -> Option<Arc<Published>> {
    let host = world.shared().services.get::<WasmHost>()?;
    let guard = host.snapshot().guard.clone();
    (!guard.rules.is_empty() || !guard.changes.is_empty()).then_some(guard)
}

fn placed(world: &mut dyn RegionWorld, pos: BlockPos) -> Option<u64> {
    world.state().peek::<Placed>()?.0.get(&pos).copied()
}

fn forget(world: &mut dyn RegionWorld, pos: BlockPos) {
    if world.state().peek::<Placed>().is_some() {
        world.state().get::<Placed>().0.remove(&pos);
    }
}

/// Enforces block rules and reports projectile hits, in every region.
pub struct Blocks;

impl RegionHooks for Blocks {
    fn allows_block_action(&self, world: &mut dyn RegionWorld, action: &PlayerBlockAction) -> bool {
        let Some(guard) = published(world) else {
            return true;
        };
        let dimension = world.dimension_info().key.clone();
        match action.action {
            BlockAction::Placed => guard.may_place(&dimension, action.pos),
            BlockAction::Broke => {
                let block = action.state.block().name().to_owned();
                let placed = placed(world, action.pos);
                guard.may_break(&dimension, action.pos, &block, placed)
            }
            BlockAction::Opened => true,
        }
    }

    fn allows_detonation(&self, world: &mut dyn RegionWorld, cause: &ExplosionCause) -> bool {
        let Some(guard) = published(world) else {
            return true;
        };
        let dimension = world.dimension_info().key.clone();
        guard.may_detonate(&dimension, cause.center)
    }

    fn allows_explosion(
        &self,
        world: &mut dyn RegionWorld,
        pos: BlockPos,
        state: BlockState,
        _cause: &ExplosionCause,
    ) -> bool {
        let Some(guard) = published(world) else {
            return true;
        };
        let dimension = world.dimension_info().key.clone();
        let block = state.block().name().to_owned();
        let placed = placed(world, pos);
        guard.may_explode(&dimension, pos, &block, placed)
    }

    fn allows_environment_change(
        &self,
        world: &mut dyn RegionWorld,
        pos: BlockPos,
        old: BlockState,
        new: BlockState,
        cause: BlockCause,
    ) -> bool {
        let Some(guard) = published(world) else {
            return true;
        };
        if guard.changes.is_empty() {
            return true;
        }
        let dimension = world.dimension_info().key.clone();
        guard.may_change(&dimension, pos, old.block(), new.block(), cause)
    }

    fn explosion_detonated(
        &self,
        world: &mut dyn RegionWorld,
        cause: &ExplosionCause,
        radius: f32,
        blocks: usize,
    ) {
        let shared = world.shared().clone();
        let Some(host) = shared.services.get::<WasmHost>() else {
            return;
        };
        if !host.listens(EventKinds::EXPLOSION) {
            return;
        }
        let [x, y, z] = cause.center;
        host.event(
            Event::Explosion(wit::Explosion {
                dimension: world.dimension_info().key.clone(),
                center: Vec3 { x, y, z },
                radius: f64::from(radius),
                blocks: blocks as u32,
                igniter: cause.igniter.as_ref().map(|igniter| uuid(igniter.uuid)),
            }),
            None,
        );
    }

    fn player_block_action(&self, world: &mut dyn RegionWorld, action: &PlayerBlockAction) {
        match action.action {
            BlockAction::Placed => {
                let Some(guard) = published(world) else {
                    return;
                };
                let dimension = world.dimension_info().key.clone();
                if guard.tracks(&dimension, action.pos) {
                    world
                        .state()
                        .get::<Placed>()
                        .0
                        .insert(action.pos, guard.serial);
                }
            }
            BlockAction::Broke => forget(world, action.pos),
            BlockAction::Opened => {}
        }
    }

    fn block_exploded(
        &self,
        world: &mut dyn RegionWorld,
        pos: BlockPos,
        _state: BlockState,
        _cause: &ExplosionCause,
    ) {
        forget(world, pos);
    }

    fn projectile_landed(&self, world: &mut dyn RegionWorld, landing: &ProjectileLanding) {
        let shared = world.shared().clone();
        let Some(host) = shared.services.get::<WasmHost>() else {
            return;
        };
        if !host.listens(EventKinds::PROJECTILE_HIT) {
            return;
        }
        let target = landing.target.and_then(|id| match world.entity(id) {
            Some(tracked) => Some(tracked.entity.base().uuid),
            None => world
                .seen_player_details()
                .into_iter()
                .find(|seen| seen.handle.id == id)
                .map(|seen| seen.uuid),
        });
        let [x, y, z] = landing.position;
        let projectile = Entity {
            uuid: uuid(landing.uuid),
            kind: landing.kind.name().to_owned(),
            pos: Vec3 { x, y, z },
            dimension: world.dimension_info().key.clone(),
        };
        let hit = Hit {
            owner: landing
                .owner
                .as_ref()
                .filter(|owner| owner.uuid != 0)
                .map(|owner| uuid(owner.uuid)),
            block: landing.block.map(|pos| wit::BlockPos {
                x: pos.x,
                y: pos.y,
                z: pos.z,
            }),
            target: target.map(uuid),
        };
        host.detailed_event(Event::ProjectileHit(projectile), None, Detail::Hit(hit));
    }
}

/// Who last hurt a player, for kill credit.
#[derive(Debug, Default)]
struct Credit {
    player: Option<(u128, u64)>,
    mob: Option<(u128, u64)>,
}

impl Credit {
    /// Records `attacker` at `tick` and returns who gets the kill then.
    fn record(&mut self, attacker: Option<&Culprit>, tick: u64) -> Option<(u128, bool)> {
        if let Some(attacker) = attacker.filter(|a| a.uuid != 0) {
            let slot = match attacker.is_player() {
                true => &mut self.player,
                false => &mut self.mob,
            };
            *slot = Some((attacker.uuid, tick));
        }
        let fresh = |slot: Option<(u128, u64)>| {
            slot.filter(|(_, at)| tick.saturating_sub(*at) <= CREDIT_TICKS)
                .map(|(uuid, _)| uuid)
        };
        fresh(self.player)
            .map(|uuid| (uuid, true))
            .or_else(|| fresh(self.mob).map(|uuid| (uuid, false)))
    }
}

/// Cancels damage between teammates and reports hurt players with what
/// hurt them.
pub struct Sessions;

impl SessionHooks for Sessions {
    fn allows_player_hurt(&self, session: &PlayerSession, source: &DamageSource) -> bool {
        let Some(attacker) = source
            .attacker
            .as_ref()
            .filter(|a| a.is_player() && a.uuid != 0 && a.uuid != session.profile.id)
        else {
            return true;
        };
        let Some(host) = session.shared.services.get::<WasmHost>() else {
            return true;
        };
        !host
            .snapshot()
            .guard
            .teammates(session.profile.id, attacker.uuid)
    }

    fn player_damaged(
        &self,
        session: &mut PlayerSession,
        by: &HurtBy,
        result: &HurtResult,
        died: bool,
    ) {
        let tick = session.tick;
        let credit = session
            .ext
            .entry::<Credit>()
            .record(by.attacker.as_ref(), tick);
        let Some(host) = session.shared.services.get::<WasmHost>() else {
            return;
        };
        if session.fake.is_some() && host.listens(EventKinds::FAKE_HURT) {
            let attacker = by.attacker.as_ref().filter(|a| a.uuid != 0);
            host.event(
                Event::FakeHurt(FakeDamage {
                    fake: uuid(session.profile.id),
                    attacker: attacker.map(|a| uuid(a.uuid)),
                    attacker_kind: attacker.map(|a| a.kind.name().to_owned()),
                    damage_type: by.kind.name.to_owned(),
                    health_lost: result.health_lost,
                }),
                None,
            );
        }
        let wanted = match died {
            true => EventKinds::PLAYER_DEATH,
            false => EventKinds::PLAYER_HURT,
        };
        if !host.listens(wanted) {
            return;
        }
        let who = crate::bridge::player(session);
        let [x, y, z] = session.pose.position;
        let pos = Vec3 { x, y, z };
        let dimension = session.shared.dimensions.get(session.dimension).key.clone();
        let damage_type = by.kind.name.to_owned();
        let event = match died {
            true => Event::PlayerDeath(Death {
                player: who.to_wit(),
                damage_type,
                pos,
                dimension,
            }),
            false => Event::PlayerHurt(Damage {
                player: who.to_wit(),
                damage_type,
                health_lost: result.health_lost,
                pos,
                dimension,
            }),
        };
        let attacker = by.attacker.as_ref().filter(|a| a.uuid != 0);
        let source = WitDamage {
            attacker: attacker.map(|a| uuid(a.uuid)),
            attacker_kind: attacker.map(|a| a.kind.name().to_owned()),
            projectile: by.projectile.map(|kind: EntityType| kind.name().to_owned()),
            credit: credit.map(|(id, _)| uuid(id)),
            credit_is_player: credit.is_some_and(|(_, player)| player),
        };
        host.detailed_event(event, Some(who), Detail::Damage(source));
    }
}

/// Cancels and reports uses of an item in the air that `air` click
/// filters match.
pub struct Uses;

impl PacketInterceptor for Uses {
    fn intercept<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        packet: &'a Serverbound,
    ) -> Pin<Box<dyn Future<Output = Verdict> + Send + 'a>> {
        let verdict = match packet {
            Serverbound::UseItem {
                off_hand: false,
                sequence,
            } => use_in_air(session, *sequence),
            _ => Verdict::Allow,
        };
        Box::pin(async move { verdict })
    }
}

fn use_in_air(session: &mut PlayerSession, sequence: i32) -> Verdict {
    let Some(host) = session.shared.services.get::<WasmHost>() else {
        return Verdict::Allow;
    };
    if !host.listens(EventKinds::USE_ITEM) {
        return Verdict::Allow;
    }
    let Some(held) = session.inventory.slots()
        [Inventory::hotbar_slot(session.inventory.selected())]
    .as_ref()
    .map(tuff_item::item::ItemSnapshot::of) else {
        return Verdict::Allow;
    };
    let matched = host.intercepts(
        ClickKind::Air,
        held.item().name(),
        |tag| {
            held.components
                .other
                .get("minecraft:custom_data")
                .and_then(|data| data.as_compound())
                .is_some_and(|data| data.get(tag).is_some())
        },
        |scope| scope.applies_to_player(session),
    );
    if !matched {
        return Verdict::Allow;
    }
    let who: Player = crate::bridge::player(session);
    let use_ = ItemUse {
        player: who.to_wit(),
        item: held.item().name().to_owned(),
        dimension: session.shared.dimensions.get(session.dimension).key.clone(),
        rotation: Rotation {
            yaw: session.pose.yaw,
            pitch: session.pose.pitch,
        },
    };
    host.event(Event::UseItem(use_), Some(who));
    session.send_inventory();
    session.send(&play::BlockChangedAck(sequence));
    Verdict::Cancel
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(place: bool, break_placed: bool, explode_placed: bool) -> Rule {
        Rule {
            dimension: "minecraft:overworld".into(),
            min: BlockPos::new(0, 0, 0),
            max: BlockPos::new(9, 9, 9),
            place,
            break_placed,
            breakable: vec!["minecraft:red_bed".into()],
            explode_placed,
            blast_proof: vec!["minecraft:glass".into()],
            cancel_explosions: false,
        }
    }

    fn published(rules: Vec<Rule>, serial: u64) -> Published {
        let mut owned = Owned::default();
        let mut counter = 0;
        let ops = rules
            .into_iter()
            .enumerate()
            .map(|(i, rule)| Op::AddRule(i as u32, rule))
            .collect();
        owned.apply(ops, &mut counter);
        Published::build([&owned].into_iter(), serial.max(counter))
    }

    const OVERWORLD: &str = "minecraft:overworld";

    #[tuff_core::watchdog]
    #[test]
    fn rules_only_cover_their_box_and_dimension() {
        let guard = published(vec![rule(false, false, false)], 1);
        let inside = BlockPos::new(5, 5, 5);
        let outside = BlockPos::new(10, 5, 5);
        assert!(!guard.may_place(OVERWORLD, inside));
        assert!(guard.may_place(OVERWORLD, outside));
        assert!(guard.may_place("minecraft:the_nether", inside));
        assert!(!guard.may_break(OVERWORLD, inside, "minecraft:stone", None));
        assert!(guard.may_break(OVERWORLD, outside, "minecraft:stone", None));
        assert!(guard.tracks(OVERWORLD, inside));
        assert!(!guard.tracks(OVERWORLD, outside));
    }

    #[tuff_core::watchdog]
    #[test]
    fn placed_blocks_break_only_when_placed_after_the_rule() {
        let guard = published(vec![rule(true, true, false)], 1);
        let pos = BlockPos::new(1, 1, 1);
        assert!(guard.may_break(OVERWORLD, pos, "minecraft:white_wool", Some(1)));
        assert!(!guard.may_break(OVERWORLD, pos, "minecraft:white_wool", Some(0)));
        assert!(!guard.may_break(OVERWORLD, pos, "minecraft:white_wool", None));
        assert!(guard.may_break(OVERWORLD, pos, "red_bed", None));
    }

    #[tuff_core::watchdog]
    #[test]
    fn explosions_spare_map_blocks_and_blast_proof_ones() {
        let guard = published(vec![rule(true, true, true)], 1);
        let pos = BlockPos::new(1, 1, 1);
        assert!(guard.may_explode(OVERWORLD, pos, "minecraft:white_wool", Some(1)));
        assert!(!guard.may_explode(OVERWORLD, pos, "minecraft:white_wool", None));
        assert!(!guard.may_explode(OVERWORLD, pos, "minecraft:glass", Some(1)));
        assert!(!guard.may_explode(OVERWORLD, pos, "minecraft:red_bed", None));
        let strict = published(vec![rule(true, true, false)], 1);
        assert!(!strict.may_explode(OVERWORLD, pos, "minecraft:white_wool", Some(1)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_covering_rule_must_allow() {
        let guard = published(vec![rule(true, true, true), rule(false, false, false)], 2);
        let pos = BlockPos::new(1, 1, 1);
        assert!(!guard.may_place(OVERWORLD, pos));
        assert!(!guard.may_break(OVERWORLD, pos, "minecraft:white_wool", Some(2)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn calls_validate_and_number_rules() {
        let mut calls = Calls::default();
        let mut ops = Vec::new();
        let wit_rule = |breakable: Vec<String>| BlockRule {
            dimension: "overworld".into(),
            min: wit::BlockPos { x: 5, y: 0, z: 5 },
            max: wit::BlockPos { x: 0, y: 3, z: 0 },
            place: true,
            break_placed: true,
            breakable,
            explode_placed: false,
            blast_proof: vec![],
            cancel_explosions: false,
        };
        let id = calls
            .add_rule(&mut ops, wit_rule(vec!["red_bed".into()]))
            .unwrap();
        assert!(
            calls
                .add_rule(&mut ops, wit_rule(vec!["nope_block".into()]))
                .is_err()
        );
        let Op::AddRule(_, parsed) = &ops[0] else {
            panic!("{ops:?}");
        };
        assert_eq!(parsed.dimension, OVERWORLD);
        assert_eq!(parsed.min, BlockPos::new(0, 0, 0));
        assert_eq!(parsed.max, BlockPos::new(5, 3, 5));
        assert_eq!(parsed.breakable, vec!["minecraft:red_bed".to_owned()]);
        assert!(calls.remove_rule(&mut ops, id));
        assert!(!calls.remove_rule(&mut ops, id));
        assert_eq!(ops.len(), 2);
    }

    #[tuff_core::watchdog]
    #[test]
    fn teams_are_per_plugin() {
        let mut serial = 0;
        let mut a = Owned::default();
        a.apply(
            vec![
                Op::Team(1, Some("red".into())),
                Op::Team(2, Some("red".into())),
                Op::Team(3, Some("blue".into())),
            ],
            &mut serial,
        );
        let mut b = Owned::default();
        b.apply(vec![Op::Team(3, Some("red".into()))], &mut serial);
        let guard = Published::build([&a, &b].into_iter(), serial);
        assert!(guard.teammates(1, 2));
        assert!(!guard.teammates(1, 3));
        a.apply(vec![Op::Team(2, None)], &mut serial);
        let guard = Published::build([&a, &b].into_iter(), serial);
        assert!(!guard.teammates(1, 2));
    }

    #[tuff_core::watchdog]
    #[test]
    fn kill_credit_prefers_a_recent_player() {
        let player = Culprit::player(1, 7, "attacker");
        let mut credit = Credit::default();
        assert_eq!(credit.record(Some(&player), 10), Some((7, true)));
        assert_eq!(credit.record(None, 110), Some((7, true)));
        assert_eq!(credit.record(None, 111), None);
    }

    fn frozen(fire_spread: bool, liquid_flow: bool, entity_grief: bool) -> Published {
        let mut owned = Owned::default();
        let mut counter = 0;
        owned.apply(
            vec![Op::AddChange(
                1,
                ChangeBox {
                    dimension: OVERWORLD.into(),
                    min: BlockPos::new(0, 0, 0),
                    max: BlockPos::new(9, 9, 9),
                    fire_spread,
                    fire_burn: false,
                    liquid_flow,
                    entity_grief,
                },
            )],
            &mut counter,
        );
        Published::build([&owned].into_iter(), counter)
    }

    #[tuff_core::watchdog]
    #[test]
    fn change_boxes_stop_fire_liquids_and_entities_inside_only() {
        let guard = frozen(false, false, false);
        let inside = BlockPos::new(2, 2, 2);
        let outside = BlockPos::new(20, 2, 2);
        let fire = BlockCause::Block(Block::Fire);
        let water = BlockCause::Block(Block::Water);
        let enderman = BlockCause::Entity {
            kind: EntityType::Enderman,
            uuid: 1,
        };
        let may = |pos, old, new, cause| guard.may_change(OVERWORLD, pos, old, new, cause);
        assert!(!may(inside, Block::Air, Block::Fire, fire));
        assert!(!may(inside, Block::OakPlanks, Block::Air, fire));
        assert!(may(inside, Block::Fire, Block::Air, fire));
        assert!(may(outside, Block::Air, Block::Fire, fire));
        assert!(!may(inside, Block::Air, Block::Water, water));
        assert!(may(inside, Block::Water, Block::Air, water));
        assert!(!may(inside, Block::Dirt, Block::Air, enderman));
        assert!(may(inside, Block::Air, Block::Stone, BlockCause::World));
    }

    #[tuff_core::watchdog]
    #[test]
    fn change_flags_allow_each_kind_separately() {
        let guard = frozen(true, true, true);
        let inside = BlockPos::new(2, 2, 2);
        let fire = BlockCause::Block(Block::Fire);
        assert!(guard.may_change(OVERWORLD, inside, Block::Air, Block::Fire, fire));
        assert!(!guard.may_change(OVERWORLD, inside, Block::OakPlanks, Block::Air, fire));
    }
}
