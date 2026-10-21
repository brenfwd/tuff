//! Mobs hurting their targets: the melee hit itself and the goal that
//! chases a target down to hit it.

use rand::Rng;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::Attribute;

use super::MobCore;
use super::goal::{Flags, Goal};
use super::physics::block_pos;
use super::target::TargetView;
use tuff_core::protocol::play;
use tuff_core::world::entity::{Culprit, Effect, EntityHit, TickContext};
use tuff_core::world::{framed, packets};

/// Ticks between two melee hits.
const ATTACK_INTERVAL: i32 = 20;
/// `MeleeAttack` only looks for a path this often.
const CAN_USE_COOLDOWN: u64 = 20;

/// Something that adjusts a mob's melee hit before it goes out (a
/// burning zombie setting its target alight).
pub type AttackHook = fn(&MobCore, &TargetView, &mut EntityHit, &mut TickContext);

impl MobCore {
    /// Swings the main hand for everyone who sees it.
    pub fn swing(&self, cx: &mut TickContext) {
        cx.effects.push(Effect::EntityPackets(packets([framed(
            &play::SwingAnimation::whack(self.base.id, 0),
        )])));
    }

    /// A melee hit on `target` with its attack damage and knockback
    /// attributes; the victim applies it (armor, shields, knockback).
    pub fn hurt_target(&mut self, target: &TargetView, cx: &mut TickContext) {
        let amount = self.attribute(Attribute::AttackDamage) as f32;
        let knockback = self.attribute(Attribute::AttackKnockback) as f32 / 2.0;
        let yaw = self.base.yaw.to_radians();
        let mut hit = EntityHit::new(amount, &damage_type::MOB_ATTACK);
        let mut attacker = Culprit::entity(&self.base);
        attacker.owner_name = self
            .tame
            .as_ref()
            .and_then(|t| t.owner)
            .and_then(|owner| cx.players.iter().find(|p| p.uuid == owner))
            .map(|p| p.name.to_string());
        hit.attacker = Some(attacker);
        hit.position = Some(self.base.position);
        hit.weapon = self.equipment.main_hand().map(tuff_item::item::ItemSnapshot::of);
        if knockback > 0.0 {
            hit.knockback = Some((
                f64::from(knockback),
                f64::from(yaw.sin()),
                -f64::from(yaw.cos()),
            ));
            let v = self.base.velocity;
            self.base.velocity = [v[0] * 0.6, v[1], v[2] * 0.6];
        }
        if let Some(hook) = self.attack_hook {
            hook(self, target, &mut hit, cx);
        }
        cx.effects.push(Effect::Hurt {
            target: target.id,
            hit: Box::new(hit),
        });
    }
}

const GIVE_UP_TICKS: u64 = 200;

/// Chases the target and hits it when in reach.
#[derive(Debug)]
pub struct MeleeAttack {
    speed: f64,
    follow_unseen: bool,
    /// Raises its arms only shortly before a hit, as zombies do.
    raise_arms: bool,
    raise_arm_ticks: i32,
    path_target: Option<[f64; 3]>,
    until_repath: i32,
    until_attack: i32,
    last_can_use: Option<u64>,
    path: Option<super::path::Path>,
    /// It gives way while the mob holds this (a skeleton with a bow).
    unless_holding: Option<tuff_vanilla_data::Item>,
    /// It closes in but never hurts (a creeper swells instead).
    harmless: bool,
    /// It may lose interest while standing in the light (spiders).
    shy: bool,
    /// Ticks between hits, when not the usual (hoglins).
    interval: Option<fn(&MobCore) -> i32>,
    /// Runs right after a hit lands (a bee marking itself stung).
    on_hit: Option<fn(&mut MobCore)>,
    /// It only goes after targets this accepts (a drowned by day).
    only_targets: Option<super::target::TargetFilter>,
    /// Lets go of a target it could not path to for 200 ticks (the brain
    /// mobs).
    gives_up: bool,
    cant_reach_since: Option<u64>,
}

impl MeleeAttack {
    pub fn new(speed: f64, follow_unseen: bool) -> Self {
        Self {
            speed,
            follow_unseen,
            raise_arms: false,
            raise_arm_ticks: 0,
            path_target: None,
            until_repath: 0,
            until_attack: 0,
            last_can_use: None,
            path: None,
            unless_holding: None,
            harmless: false,
            shy: false,
            interval: None,
            on_hit: None,
            only_targets: None,
            gives_up: false,
            cant_reach_since: None,
        }
    }

    /// Drops its target once no path to it has existed for 200 ticks.
    pub fn gives_up_unreachable(mut self) -> Self {
        self.gives_up = true;
        self
    }

    /// Only goes after targets `filter` accepts, letting go of others.
    pub fn only_targets(mut self, filter: super::target::TargetFilter) -> Self {
        self.only_targets = Some(filter);
        self
    }

    /// Runs `on_hit(mob)` right after each landed hit.
    pub fn with_on_hit(mut self, on_hit: fn(&mut MobCore)) -> Self {
        self.on_hit = Some(on_hit);
        self
    }

    /// Waits `interval(mob)` ticks between hits.
    pub fn with_interval(mut self, interval: fn(&MobCore) -> i32) -> Self {
        self.interval = Some(interval);
        self
    }

    /// The zombie flavour: arms go up just before each hit.
    pub fn zombie(speed: f64, follow_unseen: bool) -> Self {
        Self {
            raise_arms: true,
            ..Self::new(speed, follow_unseen)
        }
    }

    /// Only while the mob does not hold `item`.
    pub fn unless_holding(mut self, item: tuff_vanilla_data::Item) -> Self {
        self.unless_holding = Some(item);
        self
    }

    /// Closes in without hurting.
    pub fn harmless(mut self) -> Self {
        self.harmless = true;
        self
    }

    /// Now and then gives up on its target in the light.
    pub fn shy(mut self) -> Self {
        self.shy = true;
        self
    }

    fn attack_interval(&self) -> i32 {
        self.adjusted_delay(ATTACK_INTERVAL)
    }
}

impl Goal for MeleeAttack {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if self
            .last_can_use
            .is_some_and(|last| cx.tick.saturating_sub(last) < CAN_USE_COOLDOWN)
        {
            return false;
        }
        self.last_can_use = Some(cx.tick);
        if self.unless_holding.is_some_and(|item| mob.holding(item)) {
            return false;
        }
        let Some(target) = mob.target_view(cx).filter(|t| t.alive) else {
            return false;
        };
        if self.only_targets.is_some_and(|ok| !ok(mob, &target, cx)) {
            return false;
        }
        let path = mob.create_path(block_pos(target.position), 0, true, cx);
        let found = path.is_some();
        self.path = path;
        found || mob.within_melee_range(&target)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(target) = mob.target_view(cx).filter(|t| t.alive) else {
            return false;
        };
        if self.unless_holding.is_some_and(|item| mob.holding(item)) {
            return false;
        }
        if self.only_targets.is_some_and(|ok| !ok(mob, &target, cx)) {
            return false;
        }
        if self.gives_up
            && self
                .cant_reach_since
                .is_some_and(|since| cx.tick.saturating_sub(since) > GIVE_UP_TICKS)
        {
            mob.target = None;
            return false;
        }
        if self.shy && mob.brightness(cx) >= 0.5 && cx.rng.gen_range(0..100) == 0 {
            mob.target = None;
            return false;
        }
        if !self.follow_unseen {
            return !mob.navigation.is_done();
        }
        if !mob.within_home(block_pos(target.position)) {
            return false;
        }
        !(target.is_player() && target.creative)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let path = self.path.take();
        mob.move_along(path, self.speed, cx);
        mob.set_aggressive(true);
        self.until_repath = 0;
        self.until_attack = 0;
        self.raise_arm_ticks = 0;
        self.cant_reach_since = None;
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if mob
            .target_view(cx)
            .is_none_or(|t| t.is_player() && t.creative)
        {
            mob.target = None;
        }
        mob.set_aggressive(false);
        mob.navigation.stop();
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        mob.look.set_look_at(target.eye, 30.0, 30.0);
        self.chase(mob, &target, cx);
        self.strike(mob, &target, cx);
        if self.raise_arms {
            self.raise_arm_ticks += 1;
            let raised =
                self.raise_arm_ticks >= 5 && self.until_attack < self.attack_interval() / 2;
            mob.set_aggressive(raised);
        }
    }
}

impl MeleeAttack {
    /// Refreshes the path to `target` once the cooldown ends and the
    /// target has moved (or, rarely, anyway), backing off when far away or
    /// unreachable.
    fn chase(&mut self, mob: &mut MobCore, target: &TargetView, cx: &mut TickContext) {
        self.until_repath = (self.until_repath - 1).max(0);
        let stale = self.path_target.is_none_or(|last| {
            tuff_core::world::distance_sq(target.position, last) >= 1.0
                || cx.rng.r#gen::<f32>() < 0.05
        });
        let tracking = self.follow_unseen || mob.has_line_of_sight(target, cx);
        if !(tracking && self.until_repath <= 0 && stale) {
            return;
        }
        self.path_target = Some(target.position);
        let gap_sq = mob.distance_sq(target.position);
        let range_penalty = if gap_sq > 1024.0 {
            10
        } else if gap_sq > 256.0 {
            5
        } else {
            0
        };
        let mut wait = 4 + cx.rng.gen_range(0..7) + range_penalty;
        if mob.move_to_entity_reach(target.position, 0, self.speed, cx) {
            self.cant_reach_since = None;
        } else {
            wait += 15;
            self.cant_reach_since.get_or_insert(cx.tick);
        }
        self.until_repath = self.adjusted_delay(wait);
    }

    /// Swings at `target` when the attack cooldown is over and it is in
    /// reach and sight.
    fn strike(&mut self, mob: &mut MobCore, target: &TargetView, cx: &mut TickContext) {
        self.until_attack = (self.until_attack - 1).max(0);
        if self.until_attack > 0
            || !mob.within_melee_range(target)
            || !mob.has_line_of_sight(target, cx)
        {
            return;
        }
        self.until_attack = self
            .interval
            .map_or_else(|| self.attack_interval(), |f| f(mob));
        mob.swing(cx);
        if !self.harmless {
            mob.hurt_target(target, cx);
            if let Some(on_hit) = self.on_hit {
                on_hit(mob);
            }
        }
    }
}

/// A cat's pounce: closes in at a speed that depends on the distance and
/// bites once a second when within twice its width.
#[derive(Debug, Default)]
pub struct OcelotAttack {
    target: Option<i32>,
    attack_time: i32,
}

impl Goal for OcelotAttack {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        self.target = mob.target;
        self.target.is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(target) = self.target.and_then(|id| super::target::view(cx, id)) else {
            return false;
        };
        if !target.alive || mob.distance_sq(target.position) > 225.0 {
            return false;
        }
        !mob.navigation.is_done() || self.can_use(mob, cx)
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.target = None;
        mob.navigation.stop();
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(target) = self.target.and_then(|id| super::target::view(cx, id)) else {
            return;
        };
        mob.look.set_look_at(target.eye, 30.0, 30.0);
        let reach = f64::from(mob.size.width * 2.0 * mob.size.width * 2.0);
        let distance = mob.distance_sq(target.position);
        let speed = if distance > reach && distance < 16.0 {
            1.33
        } else if distance < 225.0 {
            0.6
        } else {
            0.8
        };
        mob.move_to_entity(target.position, speed, cx);
        self.attack_time = (self.attack_time - 1).max(0);
        if distance > reach || self.attack_time > 0 {
            return;
        }
        self.attack_time = 20;
        mob.hurt_target(&target, cx);
    }
}

/// Leaps at a target two to four blocks away.
#[derive(Debug)]
pub struct LeapAtTarget {
    height: f32,
}

impl LeapAtTarget {
    pub fn new(height: f32) -> Self {
        Self { height }
    }
}

impl Goal for LeapAtTarget {
    fn flags(&self) -> Flags {
        Flags::JUMP | Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(target) = mob.target_view(cx) else {
            return false;
        };
        let distance = mob.distance_sq(target.position);
        (4.0..=16.0).contains(&distance)
            && mob.base.on_ground
            && cx.rng.gen_range(0..super::goal::reduced_delay(5)) == 0
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        !mob.base.on_ground
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        let v = mob.base.velocity;
        let (mut dx, mut dz) = (
            target.position[0] - mob.base.position[0],
            target.position[2] - mob.base.position[2],
        );
        let length = (dx * dx + dz * dz).sqrt();
        if length * length > 1.0e-7 {
            dx = dx / length * 0.4 + v[0] * 0.2;
            dz = dz / length * 0.4 + v[2] * 0.2;
        }
        mob.base.velocity = [dx, f64::from(self.height), dz];
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tuff_core::world::entity::{EntitySummary, LivingSummary};
    use tuff_vanilla_data::registry::EntityType;

    pub(crate) fn villager(id: i32) -> EntitySummary {
        EntitySummary {
            id,
            uuid: id as u128,
            kind: EntityType::Villager,
            position: [3.5, 64.0, 0.5],
            velocity: [0.0; 3],
            yaw: 0.0,
            bounds: [3.2, 64.0, 0.2, 3.8, 65.95, 0.8],
            living: Some(LivingSummary {
                unpushable: false,
                alive: true,
                target: None,
                age: 0,
                in_love: false,
                panicking: false,
                eye_height: 1.62,
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
    fn an_unreachable_target_is_dropped_after_200_ticks_only_when_asked() {
        let targets = [villager(7)];
        crate::tests::with_entities(&targets, |mob, cx| {
            let mut patient = MeleeAttack::new(1.0, true);
            let mut giving_up = MeleeAttack::new(1.0, true).gives_up_unreachable();
            patient.cant_reach_since = Some(cx.tick);
            giving_up.cant_reach_since = Some(cx.tick);
            mob.target = Some(7);
            cx.tick += 100;
            assert!(giving_up.can_continue(mob, cx));
            cx.tick += 101;
            assert!(patient.can_continue(mob, cx));
            assert!(!giving_up.can_continue(mob, cx));
            assert_eq!(mob.target, None);
        });
    }
}

#[cfg(test)]
mod melee_pins {
    use super::*;

    fn trace(goal: MeleeAttack, other_x: f64) -> Vec<(i32, i32, bool, bool)> {
        let mut other = tests::villager(7);
        other.position[0] = other_x;
        other.bounds[0] = other_x - 0.3;
        other.bounds[3] = other_x + 0.3;
        let mut goal = goal;
        let mut out = Vec::new();
        crate::tests::with_entities(&[other], |mob, cx| {
            mob.base.position = [0.5, 64.0, 0.5];
            mob.target = Some(7);
            goal.start(mob, cx);
            for _ in 0..30 {
                goal.tick(mob, cx);
                cx.tick += 1;
                out.push((
                    goal.until_repath,
                    goal.until_attack,
                    goal.path_target.is_some(),
                    mob.aggressive,
                ));
            }
        });
        out
    }

    #[tuff_core::watchdog]
    #[test]
    fn melee_repaths_and_strikes_on_a_fixed_schedule() {
        let far: Vec<_> = (0..30).map(|t| ((24 - t).max(0), 0, true, true)).collect();
        assert_eq!(trace(MeleeAttack::new(1.0, true), 3.5), far);
        let attack = |t: i32| if t < 20 { 20 - t } else { 40 - t };
        let near: Vec<_> = (0..30)
            .map(|t| ((24 - t).max(0), attack(t), true, true))
            .collect();
        assert_eq!(trace(MeleeAttack::new(1.0, false), 1.3), near);
        let raised: Vec<_> = (0..30)
            .map(|t| ((24 - t).max(0), attack(t), true, (11..20).contains(&t)))
            .collect();
        assert_eq!(trace(MeleeAttack::zombie(1.0, false), 1.3), raised);
    }
}
