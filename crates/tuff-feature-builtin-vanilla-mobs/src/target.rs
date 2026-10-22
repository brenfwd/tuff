//! What a mob attacks: finding targets (the nearest attackable player, or
//! whoever hurt it), keeping them while they stay in range and in sight,
//! and the checks every targeting goal shares.
//!
//! A target is an id resolved each tick against the players near the
//! region and its entities, since ticking mobs only see summaries.

use rand::Rng;
use tuff_vanilla_data::Block;
use tuff_vanilla_data::registry::{Attribute, EntityType};

use super::MobCore;
use super::goal::{Flags, Goal, reduced_delay};
use super::physics::Aabb;
use tuff_core::world::entity::{Effect, NearbyPlayer, Notice, TickContext};
use tuff_core::world::player::Difficulty;
use tuff_core::world::scoreboard::Member;
use tuff_world_model::BlockPos;

/// Line of sight fails beyond this distance.
const MAX_SIGHT: f64 = 128.0;
/// However hidden, a target is noticed this close.
const MIN_VISIBILITY_DISTANCE: f64 = 2.0;
const DEFAULT_UNSEEN_MEMORY: i32 = 60;
const HURT_BY_UNSEEN_MEMORY: i32 = 300;
const ALERT_RANGE_Y: f64 = 10.0;

/// A target as the mob sees it this tick.
#[derive(Debug, Clone, Copy)]
pub struct TargetView {
    pub id: i32,
    pub uuid: u128,
    pub kind: EntityType,
    pub position: [f64; 3],
    pub bounds: Aabb,
    pub eye: [f64; 3],
    pub alive: bool,
    /// For players: in creative or spectator mode, never kept as a target
    /// by melee attackers.
    pub creative: bool,
    /// For players: not attackable (creative abilities).
    pub invulnerable: bool,
    pub in_water: bool,
    /// Mobs may target it (an axolotl playing dead may not be).
    pub seen_as_enemy: bool,
    /// The player that tamed it.
    pub owner: Option<u128>,
}

impl TargetView {
    pub fn is_player(&self) -> bool {
        self.kind == EntityType::Player
    }

    pub fn of_player(player: &NearbyPlayer) -> Self {
        Self {
            id: player.id,
            uuid: player.uuid,
            kind: EntityType::Player,
            position: player.position,
            bounds: player.bounds(),
            eye: player.eye(),
            alive: true,
            creative: player.creative,
            invulnerable: player.invulnerable,
            in_water: false,
            seen_as_enemy: true,
            owner: None,
        }
    }
}

/// Whether water touches a box, by the blocks it overlaps.
pub fn box_in_water(bounds: Aabb, cx: &TickContext) -> bool {
    let min = BlockPos::new(
        bounds[0].floor() as i32,
        bounds[1].floor() as i32,
        bounds[2].floor() as i32,
    );
    let max = BlockPos::new(
        (bounds[3] - 1.0e-7).floor() as i32,
        (bounds[4] - 1.0e-7).floor() as i32,
        (bounds[5] - 1.0e-7).floor() as i32,
    );
    (min.x..=max.x).any(|x| {
        (min.y..=max.y).any(|y| {
            (min.z..=max.z).any(|z| {
                let pos = BlockPos::new(x, y, z);
                (cx.block)(pos).is_some_and(|s| {
                    let fluid = s.fluid_state();
                    tuff_datapack::tags::is(
                        tuff_vanilla_data::tag::fluid::key::WATER,
                        fluid.fluid(),
                    ) && f64::from(y) + f64::from(fluid.own_height()) >= bounds[1]
                })
            })
        })
    })
}

/// The player or living entity `id`, if the mob can see it this tick.
pub fn view(cx: &TickContext, id: i32) -> Option<TargetView> {
    if let Some(player) = cx.players.iter().find(|p| p.id == id) {
        let mut view = TargetView::of_player(player);
        view.in_water = box_in_water(view.bounds, cx);
        return Some(view);
    }
    let entity = cx.entities.iter().find(|e| e.id == id)?;
    let living = entity.living?;
    let [x, y, z] = entity.position;
    Some(TargetView {
        id,
        uuid: entity.uuid,
        kind: entity.kind,
        position: entity.position,
        bounds: entity.bounds,
        eye: [x, y + f64::from(living.eye_height), z],
        alive: living.alive,
        creative: false,
        invulnerable: false,
        in_water: living.in_water,
        seen_as_enemy: living.seen_as_enemy,
        owner: living.owner,
    })
}

fn peaceful(cx: &TickContext) -> bool {
    *cx.shared.difficulty.borrow() == Difficulty::Peaceful
}

impl MobCore {
    /// The target it has now, if it can still see it.
    pub fn target_view(&self, cx: &TickContext) -> Option<TargetView> {
        view(cx, self.target?)
    }

    /// Whether it may attack `target` at all.
    pub fn can_attack(&self, target: &TargetView, cx: &TickContext) -> bool {
        if target.id == self.base.id
            || self.never_attacks.contains(&target.kind)
            || !target.alive
            || !target.seen_as_enemy
            || target.kind == EntityType::Ghast
        {
            return false;
        }
        if self.is_owner(target.uuid) {
            return false;
        }
        if target.is_player() && (peaceful(cx) || target.invulnerable) {
            return false;
        }
        true
    }

    /// Whether it and `target` are allies: a tamed animal and its owner,
    /// teammates (a tamed animal counts as on its owner's team), or
    /// illagers and their friends while neither is on a team.
    pub fn allied_to(&self, target: &TargetView, cx: &TickContext) -> bool {
        let own_owner = self.tame.as_ref().and_then(|t| t.owner);
        let own_root = own_owner.map(|owner| root_owner(cx, owner));
        let their_root = target.owner.map(|owner| root_owner(cx, owner));
        if own_owner == Some(target.uuid)
            || own_root == Some(target.uuid)
            || target.owner == Some(self.base.uuid)
            || their_root == Some(self.base.uuid)
        {
            return true;
        }
        let teams = cx.shared.scoreboard.teams();
        let member = |uuid: u128| team_member(cx, uuid, None);
        let mine = member(own_root.unwrap_or(self.base.uuid));
        let theirs = member(their_root.unwrap_or(target.uuid));
        if teams.allied(mine, theirs) {
            return true;
        }
        let befriends = |illager: EntityType, friend: EntityType| {
            tuff_core::world::entity_in_tag(illager, "illager")
                && tuff_core::world::entity_in_tag(friend, "illager_friends")
        };
        (befriends(self.kind(), target.kind) || befriends(target.kind, self.kind()))
            && teams.team_of(mine).is_none()
            && teams.team_of(theirs).is_none()
    }

    pub fn follow_range(&self) -> f64 {
        self.attribute(Attribute::FollowRange)
    }

    /// Whether nothing solid stands between its eyes and `target`'s.
    pub fn has_line_of_sight(&self, target: &TargetView, cx: &TickContext) -> bool {
        !self.blind && self.can_see_point(target.eye, cx)
    }

    pub fn can_see_point(&self, to: [f64; 3], cx: &TickContext) -> bool {
        let from = self.eye_position();
        if tuff_core::world::distance_sq(from, to) > MAX_SIGHT * MAX_SIGHT {
            return false;
        }
        let level = |pos: BlockPos| (cx.block)(pos).unwrap_or(Block::Stone.default_state());
        tuff_core::world::raycast::cast_colliders(from, to, &level).is_none()
    }

    /// Whether `target` is in its reach for a melee hit: its box, spread
    /// over what it rides, grown sideways by the default reach touches the
    /// target's.
    pub fn within_melee_range(&self, target: &TargetView) -> bool {
        let default_reach = 2.039_999_961_853_027_3_f64.sqrt() - 0.600_000_023_841_857_9;
        let (max_reach, min_reach) = match self
            .equipment
            .main_hand()
            .and_then(|stack| stack.item().attack_range())
        {
            Some(range) => (
                f64::from(range.max_reach * range.mob_factor),
                f64::from(range.min_reach * range.mob_factor),
            ),
            None => (default_reach, 0.0),
        };
        let mut own = self.bounds();
        if let Some(vehicle) = self.vehicle_bounds {
            own = [
                own[0].min(vehicle[0]),
                vehicle[1],
                own[2].min(vehicle[2]),
                own[3].max(vehicle[3]),
                vehicle[4],
                own[5].max(vehicle[5]),
            ];
        }
        let reaches = |reach: f64| {
            super::physics::intersects(
                super::physics::inflate(own, [reach, 0.0, reach]),
                target.bounds,
            )
        };
        reaches(max_reach) && (min_reach <= 0.0 || !reaches(min_reach))
    }

    /// The combat targeting test: attackable, within its follow range
    /// scaled by how visible the target is, and in sight unless ignored.
    fn targetable(
        &self,
        target: &TargetView,
        visibility: f64,
        sight: bool,
        cx: &TickContext,
    ) -> bool {
        self.within_home(super::physics::block_pos(target.position))
            && self.targetable_within(target, self.follow_range(), visibility, sight, cx)
    }

    /// The combat targeting test with its own range rather than the follow
    /// range (what a fish flees).
    pub fn targetable_within(
        &self,
        target: &TargetView,
        range: f64,
        visibility: f64,
        sight: bool,
        cx: &TickContext,
    ) -> bool {
        if !self.can_attack(target, cx) || self.allied_to(target, cx) {
            return false;
        }
        let range = (range * visibility).max(MIN_VISIBILITY_DISTANCE);
        if self.distance_sq(target.position) > range * range {
            return false;
        }
        !sight || self.has_line_of_sight(target, cx)
    }
}

/// Keeping a target: the part every targeting goal shares.
#[derive(Debug, Clone)]
pub(crate) struct Keeping {
    must_see: bool,
    unseen_ticks: i32,
    unseen_memory: i32,
    held: Option<i32>,
}

impl Keeping {
    pub(crate) fn new(must_see: bool) -> Self {
        Self {
            must_see,
            unseen_ticks: 0,
            unseen_memory: DEFAULT_UNSEEN_MEMORY,
            held: None,
        }
    }

    pub(crate) fn can_continue(&mut self, mob: &mut MobCore, cx: &TickContext) -> bool {
        let Some(target) = mob.target.or(self.held).and_then(|id| view(cx, id)) else {
            return false;
        };
        if !mob.can_attack(&target, cx) || mob.allied_to(&target, cx) {
            return false;
        }
        let within = mob.follow_range();
        if mob.distance_sq(target.position) > within * within {
            return false;
        }
        if self.must_see {
            if mob.has_line_of_sight(&target, cx) {
                self.unseen_ticks = 0;
            } else {
                self.unseen_ticks += 1;
                if self.unseen_ticks > reduced_delay(self.unseen_memory) {
                    return false;
                }
            }
        }
        mob.target = Some(target.id);
        true
    }

    pub(crate) fn start(&mut self) {
        self.unseen_ticks = 0;
    }

    pub(crate) fn stop(&mut self, mob: &mut MobCore) {
        mob.target = None;
        self.held = None;
    }
}

/// Which players a targeting goal considers.
pub type PlayerFilter = fn(&MobCore, &NearbyPlayer) -> bool;

/// Now and then picks the nearest attackable player in sight.
#[derive(Debug)]
pub struct NearestAttackablePlayer {
    keeping: Keeping,
    interval: i32,
    filter: Option<PlayerFilter>,
    /// Players it may pick, judged as it sees them (in water or not).
    view_filter: Option<TargetFilter>,
    found: Option<i32>,
    /// It only looks for targets while this holds (a spider in the dark).
    when: Option<fn(&MobCore, &TickContext) -> bool>,
}

impl NearestAttackablePlayer {
    pub fn new(must_see: bool) -> Self {
        Self::with_interval(10, must_see, None)
    }

    pub fn with_interval(interval: i32, must_see: bool, filter: Option<PlayerFilter>) -> Self {
        Self {
            keeping: Keeping::new(must_see),
            interval: reduced_delay(interval),
            filter,
            view_filter: None,
            found: None,
            when: None,
        }
    }

    /// Only picks players `filter` accepts as the mob sees them.
    pub fn filtered(mut self, filter: TargetFilter) -> Self {
        self.view_filter = Some(filter);
        self
    }

    /// Only looks for targets while `when` holds.
    pub fn when(mut self, when: fn(&MobCore, &TickContext) -> bool) -> Self {
        self.when = Some(when);
        self
    }

    fn find(&self, mob: &MobCore, cx: &TickContext) -> Option<i32> {
        let eye = [
            mob.base.position[0],
            mob.base.position[1] + f64::from(mob.size.eye_height),
            mob.base.position[2],
        ];
        cx.players
            .iter()
            .filter(|p| self.filter.is_none_or(|f| f(mob, p)))
            .filter(|p| {
                self.view_filter
                    .is_none_or(|f| view(cx, p.id).is_some_and(|t| f(mob, &t, cx)))
            })
            .filter(|p| {
                mob.targetable(
                    &TargetView::of_player(p),
                    p.visibility(mob.kind()),
                    true,
                    cx,
                )
            })
            .map(|p| (tuff_core::world::distance_sq(eye, p.position), p.id))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, id)| id)
    }
}

impl Goal for NearestAttackablePlayer {
    fn flags(&self) -> Flags {
        Flags::TARGET
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if self.when.is_some_and(|when| !when(mob, cx)) {
            return false;
        }
        if self.interval > 0 && cx.rng.gen_range(0..self.interval) != 0 {
            return false;
        }
        self.found = self.find(mob, cx);
        self.found.is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.keeping.can_continue(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.target = self.found;
        self.keeping.start();
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.keeping.stop(mob);
    }
}

/// Which candidates a targeting goal accepts beyond the shared checks.
pub type TargetFilter = fn(&MobCore, &TargetView, &TickContext) -> bool;

/// Now and then picks the nearest living entity of the given types (a
/// zombie's villagers), and players too when asked.
#[derive(Debug)]
pub struct NearestAttackableEntity {
    keeping: Keeping,
    kinds: &'static [EntityType],
    must_see: bool,
    players: bool,
    filter: Option<TargetFilter>,
    interval: i32,
    found: Option<i32>,
    when: Option<fn(&MobCore, &TickContext) -> bool>,
    only: Option<fn(&tuff_core::world::entity::EntitySummary) -> bool>,
    search: Option<fn(&MobCore) -> [f64; 3]>,
}

impl NearestAttackableEntity {
    pub fn any_living(must_see: bool) -> Self {
        Self::new(&[], must_see)
    }

    pub fn new(kinds: &'static [EntityType], must_see: bool) -> Self {
        Self {
            keeping: Keeping::new(must_see),
            kinds,
            must_see,
            players: false,
            filter: None,
            interval: reduced_delay(10),
            found: None,
            when: None,
            only: None,
            search: None,
        }
    }

    /// Looks for targets in the mob's box grown by `search` instead of the
    /// usual flat area.
    pub fn searching(mut self, search: fn(&MobCore) -> [f64; 3]) -> Self {
        self.search = Some(search);
        self
    }

    /// Only looks for targets while `when` holds.
    pub fn when(mut self, when: fn(&MobCore, &TickContext) -> bool) -> Self {
        self.when = Some(when);
        self
    }

    /// Only considers entities `only` accepts.
    pub fn only(mut self, only: fn(&tuff_core::world::entity::EntitySummary) -> bool) -> Self {
        self.only = Some(only);
        self
    }

    /// Players are candidates too.
    pub fn with_players(mut self) -> Self {
        self.players = true;
        self
    }

    /// Only candidates `filter` accepts.
    pub fn filtered(mut self, filter: TargetFilter) -> Self {
        self.filter = Some(filter);
        self
    }

    /// Only entities within 4 blocks up or down are looked for, so it
    /// does not set off after something in a cave far below.
    fn find(&self, mob: &MobCore, cx: &TickContext) -> Option<i32> {
        let eye = mob.eye_position();
        let range = mob.follow_range();
        let grow = self
            .search
            .map_or([range, 4.0, range], |search| search(mob));
        let area = super::physics::inflate(mob.bounds(), grow);
        let players = cx
            .players
            .iter()
            .filter(|_| self.players)
            .filter_map(|p| Some((view(cx, p.id)?, p.visibility(mob.kind()))));
        let entities = cx
            .entities
            .iter()
            .filter(|e| self.kinds.is_empty() && e.living.is_some() || self.kinds.contains(&e.kind))
            .filter(|e| self.only.is_none_or(|only| only(e)))
            .filter_map(|e| Some((view(cx, e.id)?, 1.0)));
        players
            .chain(entities)
            .filter(|(t, _)| super::physics::intersects(area, t.bounds))
            .filter(|(t, _)| self.filter.is_none_or(|f| f(mob, t, cx)))
            .filter(|(t, visibility)| mob.targetable(t, *visibility, self.must_see, cx))
            .map(|(t, _)| (tuff_core::world::distance_sq(eye, t.position), t.id))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, id)| id)
    }
}

impl Goal for NearestAttackableEntity {
    fn flags(&self) -> Flags {
        Flags::TARGET
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if self.when.is_some_and(|when| !when(mob, cx)) {
            return false;
        }
        if self.interval > 0 && cx.rng.gen_range(0..self.interval) != 0 {
            return false;
        }
        self.found = self.find(mob, cx);
        self.found.is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.keeping.can_continue(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.target = self.found;
        self.keeping.start();
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.keeping.stop(mob);
    }
}

/// Targets whoever last hurt it, optionally calling others of its kind to
/// help.
#[derive(Debug)]
pub struct HurtByTarget {
    keeping: Keeping,
    timestamp: Option<u64>,
    /// Types it calls for help, if any.
    alert: Option<&'static [EntityType]>,
    /// Attackers of these types are ignored.
    ignore: &'static [EntityType],
    when: Option<fn(&MobCore, &TickContext) -> bool>,
    /// Ticks the hurt-derived target is kept at most, if limited.
    lifetime: Option<u64>,
    pub(crate) expires_at: u64,
}

impl HurtByTarget {
    pub fn new(ignore: &'static [EntityType]) -> Self {
        Self {
            keeping: Keeping::new(true),
            timestamp: None,
            alert: None,
            ignore,
            when: None,
            lifetime: None,
            expires_at: u64::MAX,
        }
    }

    /// Drops the target after `ticks` however it is kept.
    pub fn expiring(mut self, ticks: u64) -> Self {
        self.lifetime = Some(ticks);
        self
    }

    /// Strikes back only while `when` holds.
    pub fn when(mut self, when: fn(&MobCore, &TickContext) -> bool) -> Self {
        self.when = Some(when);
        self
    }

    /// Calls mobs of `kinds` near it to attack the attacker too.
    pub fn alerting(mut self, kinds: &'static [EntityType]) -> Self {
        self.alert = Some(kinds);
        self
    }
}

impl Goal for HurtByTarget {
    fn flags(&self) -> Flags {
        Flags::TARGET
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some((attacker, at)) = mob.last_hurt_by_mob else {
            return false;
        };
        if self.timestamp == Some(at) || self.when.is_some_and(|when| !when(mob, cx)) {
            return false;
        }
        let Some(target) = view(cx, attacker) else {
            return false;
        };
        if target.is_player() && cx.shared.game_rules.borrow().bool("universal_anger") {
            return false;
        }
        if self.ignore.contains(&target.kind) {
            return false;
        }
        mob.can_attack(&target, cx) && mob.within_home(super::physics::block_pos(target.position))
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        cx.tick < self.expires_at && self.keeping.can_continue(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let (attacker, at) = mob.last_hurt_by_mob.expect("checked in can_use");
        self.expires_at = self.lifetime.map_or(u64::MAX, |t| cx.tick + t);
        mob.target = Some(attacker);
        self.keeping.held = Some(attacker);
        self.timestamp = Some(at);
        self.keeping.unseen_memory = HURT_BY_UNSEEN_MEMORY;
        if let Some(kinds) = self.alert {
            let range = mob.follow_range();
            let [x, y, z] = mob.base.position;
            let (bx, by, bz) = (x.floor(), y.floor(), z.floor());
            let area = [
                bx - range,
                by - ALERT_RANGE_Y,
                bz - range,
                bx + 1.0 + range,
                by + 1.0 + ALERT_RANGE_Y,
                bz + 1.0 + range,
            ];
            cx.effects.push(Effect::AlertNeighbours {
                kinds,
                area,
                attacker,
            });
            for other in cx.entities {
                let helps = other.id != mob.base.id
                    && kinds.contains(&other.kind)
                    && other.living.is_some_and(|l| l.alive && l.target.is_none())
                    && super::physics::intersects(other.bounds, area);
                if helps {
                    cx.effects.push(Effect::Notify {
                        target: other.id,
                        notice: Notice::Alert { attacker },
                    });
                }
            }
        }
        self.keeping.start();
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.keeping.stop(mob);
    }
}

/// A tamed, standing pet takes on whoever hurt its owner lately
/// (`by_attacker`) or whatever its owner last attacked.
#[derive(Debug)]
pub struct OwnerTarget {
    keeping: Keeping,
    by_attacker: bool,
    timestamp: Option<u64>,
    found: Option<(i32, u64)>,
}

impl OwnerTarget {
    pub fn hurt_by_attacker() -> Self {
        Self::new(true)
    }

    pub fn hurt_target() -> Self {
        Self::new(false)
    }

    fn new(by_attacker: bool) -> Self {
        Self {
            keeping: Keeping::new(false),
            by_attacker,
            timestamp: None,
            found: None,
        }
    }

    fn wants_to_attack(target: &TargetView, owner: u128, cx: &TickContext) -> bool {
        match target.kind {
            EntityType::Creeper | EntityType::Ghast | EntityType::ArmorStand => return false,
            EntityType::Wolf => {
                let tamed_by_owner = target.owner == Some(owner);
                return !tamed_by_owner;
            }
            EntityType::Player if !cx.shared.game_rules.borrow().bool("pvp") => return false,
            _ => {}
        }
        !cx.entities
            .iter()
            .any(|e| e.id == target.id && e.living.is_some_and(|l| l.tamed))
    }
}

impl Goal for OwnerTarget {
    fn flags(&self) -> Flags {
        Flags::TARGET
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(tame) = mob.tame.as_ref() else {
            return false;
        };
        let Some(owner_uuid) = tame.owner.filter(|_| tame.is_tamed() && !tame.sitting) else {
            return false;
        };
        let Some(owner) = cx.players.iter().find(|p| p.uuid == owner_uuid) else {
            return false;
        };
        let recorded = if self.by_attacker {
            owner.last_hurt_by
        } else {
            owner.last_hurt_mob
        };
        let Some((id, at)) = recorded else {
            return false;
        };
        if self.timestamp == Some(at) {
            return false;
        }
        let Some(target) = view(cx, id) else {
            return false;
        };
        if !mob.targetable(&target, 1.0, false, cx)
            || !Self::wants_to_attack(&target, owner_uuid, cx)
        {
            return false;
        }
        self.found = Some((id, at));
        true
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.keeping.can_continue(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        if let Some((id, at)) = self.found.take() {
            mob.target = Some(id);
            self.keeping.held = Some(id);
            self.timestamp = Some(at);
        }
        self.keeping.start();
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.keeping.stop(mob);
    }
}

/// Under the universal anger rule, turns a player-hurt neutral mob (and
/// optionally its kin nearby) against every player.
#[derive(Debug, Default)]
pub struct ResetUniversalAnger {
    alert: bool,
    seen: Option<u64>,
}

impl ResetUniversalAnger {
    pub fn new(alert_others: bool) -> Self {
        Self {
            alert: alert_others,
            seen: None,
        }
    }

    fn attacker(mob: &MobCore, cx: &TickContext) -> Option<(TargetView, u64)> {
        let (id, at) = mob.last_hurt_by_mob?;
        view(cx, id).filter(|t| t.is_player()).map(|t| (t, at))
    }
}

impl Goal for ResetUniversalAnger {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        cx.shared.game_rules.borrow().bool("universal_anger")
            && Self::attacker(mob, cx).is_some_and(|(_, at)| self.seen != Some(at))
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some((attacker, at)) = Self::attacker(mob, cx).map(|(t, at)| (t.uuid, at)) else {
            return;
        };
        self.seen = Some(at);
        mob.refresh_universal_anger(attacker, cx);
        if !self.alert {
            return;
        }
        let range = mob.follow_range();
        let [x, y, z] = mob.base.position;
        let (bx, by, bz) = (x.floor(), y.floor(), z.floor());
        let area = [
            bx - range,
            by - ALERT_RANGE_Y,
            bz - range,
            bx + 1.0 + range,
            by + 1.0 + ALERT_RANGE_Y,
            bz + 1.0 + range,
        ];
        for other in cx.entities {
            if other.id != mob.base.id
                && other.kind == mob.base.kind
                && super::physics::intersects(other.bounds, area)
            {
                cx.effects.push(Effect::Notify {
                    target: other.id,
                    notice: Notice::UniversalAnger { attacker },
                });
            }
        }
    }
}

/// A neutral mob's grudge: whom it is angry at and for how much longer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anger {
    pub target: u128,
    pub ticks: i32,
    /// Angry at every player (the universal anger rule), not just `target`.
    pub universal: bool,
}

impl MobCore {
    /// Takes a saved grudge back up: still angry until `end`, and
    /// targeting the angering player or entity when it is around.
    pub(super) fn restore_anger(&mut self, end: i64, target: u128, cx: &TickContext) {
        let ticks = end - cx.tick as i64;
        if ticks <= 0 {
            return;
        }
        self.anger = Some(Anger {
            target,
            ticks: ticks.min(i64::from(i32::MAX)) as i32,
            universal: false,
        });
        self.target = cx
            .players
            .iter()
            .find(|p| p.uuid == target)
            .map(|p| p.id)
            .or_else(|| cx.entities.iter().find(|e| e.uuid == target).map(|e| e.id));
    }

    /// Forgets the grudge, the attacker and the target.
    pub fn stop_being_angry(&mut self) {
        self.anger = None;
        self.last_hurt_by_mob = None;
        self.target = None;
    }

    /// A player died: a neutral mob near the death forgives them if they
    /// angered it, and a piglin forgets its grudge wherever it is.
    pub fn player_died(&mut self, player: u128, nearby: bool) {
        let piglin = matches!(self.base.kind, EntityType::Piglin | EntityType::PiglinBrute);
        if !nearby && !piglin {
            return;
        }
        if self
            .anger_loaded
            .is_some_and(|(_, target)| target == player)
        {
            self.anger_loaded = None;
        }
        if !self
            .anger
            .is_some_and(|a| a.target == player && !a.universal)
        {
            return;
        }
        if piglin {
            self.anger = None;
        } else {
            self.stop_being_angry();
        }
    }

    /// Whether it holds a grudge against `target`.
    pub fn is_angry_at(&self, target: &TargetView, cx: &TickContext) -> bool {
        self.can_attack(target, cx)
            && self
                .anger
                .is_some_and(|a| a.target == target.uuid || (a.universal && target.is_player()))
    }

    /// Whether its grudge covers the player `uuid`.
    pub fn angry_at_player(&self, uuid: u128) -> bool {
        self.anger.is_some_and(|a| a.target == uuid || a.universal)
    }

    /// Drops the target and starts a fresh grudge against every player.
    pub fn refresh_universal_anger(&mut self, attacker: u128, cx: &mut TickContext) {
        self.stop_being_angry();
        self.anger = Some(Anger {
            target: attacker,
            ticks: cx.rng.gen_range(400..=780),
            universal: true,
        });
    }

    /// Keeps the grudge in step with the target: a new target starts 20
    /// to 39 seconds of anger, which runs out while no player is targeted.
    pub fn update_anger(&mut self, stop_if_no_target: bool, cx: &mut TickContext) {
        if let Some(anger) = self.anger
            && cx
                .players
                .iter()
                .find(|p| p.uuid == anger.target)
                .is_some_and(|p| p.creative || p.invulnerable || peaceful(cx))
        {
            self.stop_being_angry();
            return;
        }
        let target = self.target_view(cx).filter(|t| t.alive);
        if target.is_none()
            && let Some(anger) = self.anger
            && cx
                .entities
                .iter()
                .any(|e| e.uuid == anger.target && e.living.is_some())
        {
            self.stop_being_angry();
            return;
        }
        if let Some(target) = target
            && self
                .anger
                .is_none_or(|a| a.target != target.uuid && !(a.universal && target.is_player()))
        {
            self.anger = Some(Anger {
                target: target.uuid,
                ticks: cx.rng.gen_range(400..=780),
                universal: false,
            });
        }
        let player_target = target.is_some_and(|t| t.is_player());
        if let Some(anger) = &mut self.anger
            && anger.ticks > 0
            && (!player_target || !stop_if_no_target)
        {
            anger.ticks -= 1;
            if anger.ticks == 0 {
                self.stop_being_angry();
            }
        }
    }
}

/// Follows the owner chain from `owner` to the entity that owns nothing
/// itself (a tamed animal owned by a tamed animal resolves to the
/// player at the end).
pub(crate) fn root_owner(cx: &TickContext, owner: u128) -> u128 {
    let mut root = owner;
    for _ in 0..16 {
        let next = cx
            .entities
            .iter()
            .find(|e| e.uuid == root)
            .and_then(|e| e.living.and_then(|l| l.owner));
        match next {
            Some(next) if next != root => root = next,
            _ => break,
        }
    }
    root
}

/// The team identity of an entity: a tamed animal takes its owner's.
pub(crate) fn team_member<'a>(cx: &'a TickContext, uuid: u128, owner: Option<u128>) -> Member<'a> {
    let uuid = owner.unwrap_or(uuid);
    match cx.players.iter().find(|p| p.uuid == uuid) {
        Some(player) => Member::Player(player.name),
        None => Member::Entity(uuid),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tuff_core::world::entity::{EntityBase, EntitySummary, LivingSummary};

    pub(crate) fn zombie(id: i32, position: [f64; 3]) -> EntitySummary {
        EntitySummary {
            id,
            velocity: [0.0; 3],
            yaw: 0.0,
            uuid: id as u128,
            kind: EntityType::Zombie,
            position,
            bounds: [
                position[0] - 0.3,
                position[1],
                position[2] - 0.3,
                position[0] + 0.3,
                position[1] + 1.95,
                position[2] + 0.3,
            ],
            living: Some(LivingSummary {
                unpushable: false,
                alive: true,
                target: None,
                age: 0,
                in_love: false,
                panicking: false,
                eye_height: 1.74,
                in_water: false,
                seen_as_enemy: true,
                enemy: false,
                health: 20.0,
                tamed: false,
                owner: None,
                chasing: None,
            }),
            pickable: true,
            item: None,
            vehicle: None,
            carries: false,
            ominous_banner: false,
            blue_sheep: false,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_rider_reaches_from_the_whole_box_of_its_vehicle() {
        let mut rider = MobCore::new(EntityBase::new(
            EntityType::Vindicator,
            1,
            1,
            [0.0, 2.2, 0.0],
            [0.0; 3],
        ));
        let mut target = view_of(9, EntityType::Zombie, None);
        target.bounds = [1.6, 0.0, -0.3, 2.2, 1.95, 0.3];
        assert!(!rider.within_melee_range(&target));
        rider.vehicle_bounds = Some([-0.975, 0.0, -0.975, 0.975, 2.2, 0.975]);
        assert!(rider.within_melee_range(&target));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_held_attack_range_scales_the_melee_reach_by_the_mob_factor() {
        let mut mob = MobCore::new(EntityBase::new(
            EntityType::Vindicator,
            1,
            1,
            [0.0, 0.0, 0.0],
            [0.0; 3],
        ));
        let mut near = view_of(9, EntityType::Zombie, None);
        near.bounds = [0.9, 0.0, -0.3, 1.5, 1.95, 0.3];
        let mut far = view_of(10, EntityType::Zombie, None);
        far.bounds = [1.6, 0.0, -0.3, 2.2, 1.95, 0.3];
        assert!(mob.within_melee_range(&near));
        assert!(!mob.within_melee_range(&far));
        mob.equipment.set(
            tuff_vanilla_data::item::EquipmentSlot::Mainhand,
            Some(tuff_item::item::ItemStack::mint(
                tuff_vanilla_data::Item::WoodenSpear,
                1,
                tuff_item::item::MintReason::Test,
            )),
        );
        assert!(!mob.within_melee_range(&near));
        assert!(mob.within_melee_range(&far));
    }

    #[tuff_core::watchdog]
    #[test]
    fn species_shrug_off_their_immune_effects() {
        use tuff_core::world::effects::ActiveEffect;
        use tuff_vanilla_data::registry::MobEffect;
        let core = |kind| MobCore::new(EntityBase::new(kind, 1, 1, [0.0; 3], [0.0; 3]));
        let mut wither_skeleton = core(EntityType::WitherSkeleton);
        assert!(!wither_skeleton.add_effect(ActiveEffect::timed(MobEffect::Wither, 100, 0)));
        assert!(wither_skeleton.add_effect(ActiveEffect::timed(MobEffect::Weakness, 100, 0)));
        assert!(!core(EntityType::Parched).can_be_affected(MobEffect::Weakness));
        assert!(core(EntityType::Parched).can_be_affected(MobEffect::Wither));
        assert!(!core(EntityType::Spider).can_be_affected(MobEffect::Poison));
        assert!(!core(EntityType::CaveSpider).can_be_affected(MobEffect::Poison));
        assert!(core(EntityType::Zombie).can_be_affected(MobEffect::Weakness));
    }

    fn view_of(uuid: u128, kind: EntityType, owner: Option<u128>) -> TargetView {
        TargetView {
            id: 5,
            uuid,
            kind,
            position: [0.0; 3],
            bounds: [0.0; 6],
            eye: [0.0; 3],
            alive: true,
            creative: false,
            invulnerable: false,
            in_water: false,
            seen_as_enemy: true,
            owner,
        }
    }

    fn angry(kind: EntityType, at: u128) -> MobCore {
        let mut core = MobCore::new(EntityBase::new(kind, 1, 1, [0.0; 3], [0.0; 3]));
        core.anger = Some(Anger {
            target: at,
            ticks: 400,
            universal: false,
        });
        core.target = Some(9);
        core.last_hurt_by_mob = Some((9, 3));
        core
    }

    #[tuff_core::watchdog]
    #[test]
    fn neutral_mobs_forgive_a_dead_player_only_near_the_death() {
        for kind in [
            EntityType::IronGolem,
            EntityType::Bee,
            EntityType::PolarBear,
            EntityType::Enderman,
            EntityType::ZombifiedPiglin,
        ] {
            let mut core = angry(kind, 42);
            core.player_died(7, true);
            assert!(core.anger.is_some(), "{kind:?} forgave someone else");
            core.player_died(42, false);
            assert!(core.anger.is_some(), "{kind:?} forgave from afar");
            core.player_died(42, true);
            assert_eq!(core.anger, None, "{kind:?}");
            assert_eq!((core.target, core.last_hurt_by_mob), (None, None));

            let mut loaded = MobCore::new(EntityBase::new(kind, 1, 1, [0.0; 3], [0.0; 3]));
            loaded.anger_loaded = Some((1_000, 42));
            loaded.player_died(42, true);
            assert_eq!(loaded.anger_loaded, None, "{kind:?} saved grudge");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn piglins_forget_a_dead_player_anywhere_but_keep_their_target() {
        let mut core = angry(EntityType::Piglin, 42);
        core.player_died(42, false);
        assert_eq!(core.anger, None);
        assert_eq!(core.target, Some(9));
    }

    #[tuff_core::watchdog]
    #[test]
    fn tamed_animals_and_their_owners_are_allies_both_ways() {
        crate::tests::with_context(|core, cx| {
            let stranger = view_of(42, EntityType::Player, None);
            assert!(!core.allied_to(&stranger, cx));
            let pet = view_of(43, EntityType::Wolf, Some(core.base.uuid));
            assert!(core.allied_to(&pet, cx), "its own pet");
            core.make_tameable(18);
            core.try_tame(99, 1.0, cx);
            assert!(core.allied_to(&view_of(99, EntityType::Player, None), cx));
            assert!(!core.allied_to(&stranger, cx));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_pet_owned_by_a_pet_is_allied_with_the_root_owner() {
        let mut middle = zombie(50, [0.0; 3]);
        middle.living.as_mut().unwrap().owner = Some(51);
        crate::tests::with_entities(&[middle], |core, cx| {
            core.make_tameable(18);
            core.try_tame(50, 1.0, cx);
            assert!(core.allied_to(&view_of(51, EntityType::Player, None), cx));
            assert!(!core.allied_to(&view_of(52, EntityType::Player, None), cx));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn illagers_befriend_their_friends_off_teams() {
        crate::tests::with_context(|core, cx| {
            core.base.kind = EntityType::Pillager;
            let vindicator = view_of(44, EntityType::Vindicator, None);
            assert!(core.allied_to(&vindicator, cx));
            assert!(!core.allied_to(&view_of(45, EntityType::Ravager, None), cx));
            assert!(!core.allied_to(&view_of(46, EntityType::Villager, None), cx));
            core.base.kind = EntityType::Cow;
            assert!(!core.allied_to(&vindicator, cx));
        });
    }

    fn nearest_found(entities: &[EntitySummary]) -> Option<i32> {
        let shared = tuff_test_support::bare_shared();
        let block = |_: BlockPos| Some(Block::Air.default_state());
        let light = |_: BlockPos| Some((15, 0));
        let biome = |_: BlockPos| Some("minecraft:plains");
        let mut rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(1);
        let mut effects = Vec::new();
        let mut budget = 10_000;
        let pois = tuff_core::world::poi::PoiStore::default();
        let cx = TickContext {
            tick: 1,
            dimension: tuff_core::world::dimension::DimensionId::for_tests(0),
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
            parts: &[],
            pistons: &[],
            shared: &shared,
            rng: &mut rng,
            effects: &mut effects,
            path_budget: &mut budget,
            pois: &pois,
            state: &tuff_core::world::region_api::RegionStateLock::default(),
        };
        let base = EntityBase::new(EntityType::IronGolem, 1, 1, [0.5, 64.0, 0.5], [0.0; 3]);
        let core = MobCore::new(base);
        NearestAttackableEntity::new(&[EntityType::Zombie], false).find(&core, &cx)
    }

    #[tuff_core::watchdog]
    #[test]
    fn entity_targets_are_looked_for_only_a_few_blocks_up_or_down() {
        assert_eq!(nearest_found(&[zombie(7, [8.5, 54.0, 0.5])]), None);
        assert_eq!(nearest_found(&[zombie(7, [8.5, 66.0, 0.5])]), Some(7));
    }
}
