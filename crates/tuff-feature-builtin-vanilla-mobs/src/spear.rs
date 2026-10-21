//! Mobs holding a spear: they close in, lower the spear for the charge,
//! stab whatever it touches while they run, then back away and go again.
//! A mounted mob charges at its vehicle's charge speed.

use tuff_item::item::DupUnchecked;
use tuff_vanilla_data::registry::{Attribute, EntityType};
use tuff_vanilla_data::{Block, damage_type};

use super::MobCore;
use super::goal::{Flags, Goal};
use super::random_pos::land_pos_away_between;
use tuff_core::world::entity::{Culprit, Effect, EntityHit, TickContext};
use tuff_core::world::raycast;

const REPOSITION_MIN: f64 = 6.0;
const REPOSITION_MAX: f64 = 7.0;
const COOLDOWN_MIN: f64 = 9.0;
const COOLDOWN_MAX: f64 = 11.0;
const POS_VERTICAL: i32 = 7;
const MAX_FLEEING_TICKS: i32 = 100;
const MOUNTED_EXTRA: f64 = 2.0;
/// How strongly a mob's stab counts against a target's speed, against a
/// player's full strength.
const MOB_ACTION_FACTOR: f64 = 0.2;
const STAB_KNOCKBACK: f64 = 0.4;

/// How much faster a mob rides a `vehicle` when it charges.
pub fn charge_speed_modifier(vehicle: EntityType) -> f64 {
    match vehicle {
        EntityType::CamelHusk => 4.0,
        EntityType::ZombieHorse => 1.4,
        _ => 1.0,
    }
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Where along the ray the box grown by `margin` is first inside
/// `[from, to]`, if it is.
fn ray_enters(
    origin: [f64; 3],
    direction: [f64; 3],
    bounds: [f64; 6],
    margin: f64,
    from: f64,
    to: f64,
) -> Option<f64> {
    let (mut near, mut far) = (from, to);
    for axis in 0..3 {
        let low = bounds[axis] - margin;
        let high = bounds[axis + 3] + margin;
        if direction[axis].abs() < 1.0e-9 {
            if origin[axis] < low || origin[axis] > high {
                return None;
            }
            continue;
        }
        let (a, b) = (
            (low - origin[axis]) / direction[axis],
            (high - origin[axis]) / direction[axis],
        );
        near = near.max(a.min(b));
        far = far.min(a.max(b));
        if near > far {
            return None;
        }
    }
    Some(near)
}

fn holds_spear(mob: &MobCore) -> bool {
    mob.equipment
        .main_hand()
        .is_some_and(|s| s.kinetic_weapon().is_some())
}

/// The topmost entity `mob` rides, if it rides anything.
fn root_vehicle(mob: &MobCore, cx: &TickContext) -> Option<(i32, EntityType)> {
    let mut at = mob.base.vehicle.as_ref()?.id;
    let mut kind = EntityType::Player;
    for _ in 0..8 {
        let summary = cx.entities.iter().find(|e| e.id == at)?;
        kind = summary.kind;
        match summary.vehicle {
            Some(next) => at = next,
            None => break,
        }
    }
    Some((at, kind))
}

fn root_of(id: i32, cx: &TickContext) -> i32 {
    let mut at = id;
    for _ in 0..8 {
        match cx
            .entities
            .iter()
            .find(|e| e.id == at)
            .and_then(|e| e.vehicle)
        {
            Some(next) => at = next,
            None => break,
        }
    }
    at
}

#[derive(Debug, Default)]
struct Run {
    away: Option<[f64; 3]>,
    engage: Option<i32>,
    fleeing: i32,
    done: bool,
    used: u64,
}

/// Approach a target, charge it with the held spear, back off, repeat.
#[derive(Debug)]
pub struct SpearUse {
    charging_speed: f64,
    repositioning_speed: f64,
    approach_sq: f64,
    in_range_sq: f64,
    run: Option<Run>,
    stabbed: Vec<(i32, u64)>,
    last_position: Option<[f64; 3]>,
    motion: [f64; 3],
}

impl SpearUse {
    pub fn new(
        charging_speed: f64,
        repositioning_speed: f64,
        approach_distance: f64,
        in_range_radius: f64,
    ) -> Self {
        Self {
            charging_speed,
            repositioning_speed,
            approach_sq: approach_distance * approach_distance,
            in_range_sq: in_range_radius * in_range_radius,
            run: None,
            stabbed: Vec::new(),
            last_position: None,
            motion: [0.0; 3],
        }
    }

    /// The goal the zombie family and piglins use.
    pub fn standard() -> Self {
        Self::new(1.0, 1.0, 10.0, 2.0)
    }

    fn able(mob: &MobCore, cx: &TickContext) -> bool {
        holds_spear(mob) && mob.target_view(cx).is_some_and(|t| t.alive)
    }

    fn use_duration(mob: &MobCore) -> i32 {
        mob.equipment
            .main_hand()
            .and_then(|s| s.kinetic_weapon())
            .map_or(0, |k| {
                (k.delay_ticks + k.damage_conditions.map_or(0, |c| c.max_duration_ticks)) as i32
            })
    }

    fn track_motion(&mut self, mob: &MobCore) {
        let now = mob.base.position;
        if let Some(last) = self.last_position {
            self.motion = [0, 1, 2].map(|i| (now[i] - last[i]) * 20.0);
        }
        self.last_position = Some(now);
    }

    fn retreat_pos(
        mob: &MobCore,
        target: [f64; 3],
        distance: f64,
        mounted: f64,
        cx: &mut TickContext,
    ) -> Option<[f64; 3]> {
        let lo = (COOLDOWN_MIN + mounted - distance).max(0.0);
        let hi = (COOLDOWN_MAX + mounted - distance).max(1.0);
        land_pos_away_between(mob, lo, hi, POS_VERTICAL, target, cx)
    }

    fn reposition_pos(
        mob: &MobCore,
        target: [f64; 3],
        distance: f64,
        mounted: f64,
        cx: &mut TickContext,
    ) -> Option<[f64; 3]> {
        let lo = REPOSITION_MIN + mounted - distance;
        let hi = REPOSITION_MAX + mounted - distance;
        land_pos_away_between(mob, lo, hi, POS_VERTICAL, target, cx)
    }

    fn sweep(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(stack) = mob.equipment.main_hand().map(DupUnchecked::dup_unchecked) else {
            return;
        };
        let (Some(kinetic), Some(mut range)) =
            (stack.kinetic_weapon(), stack.item.attack_range().copied())
        else {
            return;
        };
        let used = self.run.as_ref().map_or(0, |r| r.used);
        if used < kinetic.delay_ticks {
            return;
        }
        let used = used - kinetic.delay_ticks;
        range.min_reach *= range.mob_factor;
        range.max_reach *= range.mob_factor;
        let eye = mob.eye_position();
        let forward = raycast::view_vector(mob.base.yaw, mob.base.pitch);
        let speed = dot(self.motion, forward);
        let own_root = root_vehicle(mob, cx).map_or(mob.base.id, |(id, _)| id);
        let level = |pos| (cx.block)(pos).unwrap_or(Block::Air.default_state());
        let clear = |along: f64| {
            let to = [0, 1, 2].map(|i| eye[i] + forward[i] * along);
            raycast::cast_colliders(eye, to, &level).is_none()
        };
        let mut victims: Vec<(i32, [f64; 3], bool)> = Vec::new();
        let margin = f64::from(range.hitbox_margin);
        let (min_reach, max_reach) = (f64::from(range.min_reach), f64::from(range.max_reach));
        let mut consider =
            |id: i32, bounds: [f64; 6], motion: [f64; 3], dismountable: bool| {
                if self.stabbed.iter().any(|(other, at)| {
                    *other == id && cx.tick - at < kinetic.contact_cooldown_ticks
                }) {
                    return;
                }
                let Some(along) = ray_enters(
                    eye,
                    forward,
                    bounds,
                    margin,
                    (min_reach - margin).max(0.0),
                    max_reach + margin,
                ) else {
                    return;
                };
                if clear(along) {
                    victims.push((id, motion, dismountable));
                }
            };
        for p in cx.players {
            if p.invulnerable || p.vehicle.is_some_and(|v| root_of(v, cx) == own_root) {
                continue;
            }
            consider(
                p.id,
                p.bounds(),
                p.movement.map(|c| c * 20.0),
                p.vehicle.is_some(),
            );
        }
        for e in cx.entities {
            if e.id == mob.base.id
                || e.id == own_root
                || !(e.pickable || e.living.is_some())
                || e.item.is_some()
                || root_of(e.id, cx) == own_root
            {
                continue;
            }
            let motion = cx
                .entities
                .iter()
                .find(|v| v.id == root_of(e.id, cx))
                .map_or(e.velocity, |v| v.velocity);
            let dismountable = e.vehicle.is_some()
                && !tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::entity_type::key::CANNOT_BE_DISMOUNTED_BY_ITEM_USAGE,
                    e.kind,
                );
            consider(e.id, e.bounds, motion.map(|c| c * 20.0), dismountable);
        }
        let knockback_base = mob.attribute(Attribute::AttackKnockback);
        let base_damage = mob.attribute(Attribute::AttackDamage);
        let yaw = f64::from(mob.base.yaw.to_radians());
        let kind = stack.item.damage_type().unwrap_or(&damage_type::MOB_ATTACK);
        let test = |c: &Option<tuff_vanilla_data::item::KineticCondition>, relative: f64| {
            c.as_ref()
                .is_some_and(|c| c.test(used, speed, relative, MOB_ACTION_FACTOR))
        };
        let mut weapon = stack.dup_unchecked();
        let wear = stack
            .weapon()
            .map_or(0, |w| w.item_damage_per_attack as u32);
        let (mut broken, mut worn) = (false, false);
        for (id, motion, dismountable) in victims {
            self.stabbed.retain(|(other, _)| *other != id);
            self.stabbed.push((id, cx.tick));
            let relative = (speed - dot(motion, forward)).max(0.0);
            let damages = test(&kinetic.damage_conditions, relative);
            let knocks = test(&kinetic.knockback_conditions, relative);
            let dismounts = dismountable && test(&kinetic.dismount_conditions, relative);
            if !(damages || knocks || test(&kinetic.dismount_conditions, relative)) {
                continue;
            }
            if dismounts {
                cx.effects.push(Effect::StabDismount { target: id });
            }
            let living = cx.players.iter().any(|p| p.id == id)
                || cx.entities.iter().any(|e| e.id == id && e.living.is_some());
            if living && !broken && wear > 0 {
                worn = true;
                broken = weapon.hurt(wear, cx.rng) == tuff_item::item::Hurt::Broken;
            }
            if !(damages || knocks) {
                continue;
            }
            let amount = if damages {
                base_damage as f32
                    + (relative * f64::from(kinetic.damage_multiplier)).floor() as f32
            } else {
                0.0
            };
            let mut hit = EntityHit::new(amount, kind);
            let mut attacker = Culprit::entity(&mob.base);
            attacker.owner_name = None;
            hit.attacker = Some(attacker);
            hit.position = Some(mob.base.position);
            hit.weapon = Some(stack.dup_unchecked());
            if knocks {
                hit.knockback =
                    Some((STAB_KNOCKBACK + knockback_base / 2.0, yaw.sin(), -yaw.cos()));
            }
            cx.effects.push(Effect::Hurt {
                target: id,
                hit: Box::new(hit),
            });
        }
        if broken {
            mob.equipment
                .set(tuff_vanilla_data::item::EquipmentSlot::Mainhand, None);
        } else if worn {
            mob.equipment.set(
                tuff_vanilla_data::item::EquipmentSlot::Mainhand,
                Some(weapon),
            );
        }
    }
}

impl Goal for SpearUse {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        Self::able(mob, cx) && mob.using_item.is_none()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.run.as_ref().is_some_and(|r| !r.done) && Self::able(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.set_aggressive(true);
        self.run = Some(Run::default());
        self.stabbed.clear();
        self.last_position = Some(mob.base.position);
        self.motion = [0.0; 3];
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
        mob.set_aggressive(false);
        mob.stop_using_item();
        self.run = None;
        self.stabbed.clear();
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.track_motion(mob);
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        if self.run.is_none() {
            return;
        }
        let distance_sq = mob.distance_sq(target.position);
        let distance = distance_sq.sqrt();
        let charge = root_vehicle(mob, cx).map_or(1.0, |(_, kind)| charge_speed_modifier(kind));
        let mounted = if mob.base.vehicle.is_some() {
            MOUNTED_EXTRA
        } else {
            0.0
        };
        mob.turn_to(target.eye, 30.0, 30.0);
        mob.look.set_look_at(target.eye, 30.0, 30.0);

        if self.run.as_ref().is_some_and(|r| r.engage.is_none()) {
            if distance_sq > self.approach_sq {
                mob.move_to_entity(target.position, charge * self.repositioning_speed, cx);
                return;
            }
            if let Some(run) = &mut self.run {
                run.engage = Some(Self::use_duration(mob));
                run.used = 0;
            }
            mob.start_using_item();
        }

        if mob.using_item.is_some() {
            self.sweep(mob, cx);
            if let Some(run) = &mut self.run {
                run.used += 1;
            }
        }

        let mut retreat = false;
        if let Some(run) = &mut self.run
            && let Some(engage) = run.engage.as_mut()
            && *engage > 0
        {
            *engage -= 1;
            retreat = *engage == 0;
        }
        if retreat {
            mob.stop_using_item();
            let away = Self::retreat_pos(mob, target.position, distance, mounted, cx);
            if let Some(run) = &mut self.run {
                run.away = away;
                run.fleeing = 1;
            }
        }

        let Some(run) = &mut self.run else {
            return;
        };
        if run.fleeing > 0 {
            run.fleeing += 1;
            if run.fleeing > MAX_FLEEING_TICKS {
                run.done = true;
                return;
            }
        }
        if let Some(away) = run.away {
            mob.move_to_position(away, charge * self.repositioning_speed, cx);
            if mob.navigation.is_done() {
                if run.fleeing > 0 {
                    run.done = true;
                } else {
                    run.away = None;
                }
            }
            return;
        }
        mob.move_to_entity(target.position, charge * self.charging_speed, cx);
        if distance_sq < self.in_range_sq || mob.navigation.is_done() {
            let away = Self::reposition_pos(mob, target.position, distance, mounted, cx);
            if let Some(run) = &mut self.run {
                run.away = away;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn mounts_charge_at_their_own_speed_modifier() {
        assert_eq!(charge_speed_modifier(EntityType::CamelHusk), 4.0);
        assert_eq!(charge_speed_modifier(EntityType::ZombieHorse), 1.4);
        assert_eq!(charge_speed_modifier(EntityType::Chicken), 1.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_stab_ray_hits_a_box_within_reach_only() {
        let bounds = [4.0, 0.0, -0.3, 4.6, 1.8, 0.3];
        let eye = [0.0, 1.0, 0.0];
        let ahead = [1.0, 0.0, 0.0];
        assert!(ray_enters(eye, ahead, bounds, 0.125, 0.0, 5.0).is_some());
        assert!(ray_enters(eye, ahead, bounds, 0.125, 0.0, 3.0).is_none());
        assert!(ray_enters(eye, [0.0, 0.0, 1.0], bounds, 0.125, 0.0, 5.0).is_none());
        assert!(ray_enters([0.0, 3.0, 0.0], ahead, bounds, 0.125, 0.0, 5.0).is_none());
    }
}
