//! Breezes: they leap behind their target, slide around it in rings, and
//! blast wind charges at whatever they can see in range.

use rand::Rng;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use tuff_vanilla_data::entity::Pose;
use tuff_vanilla_data::registry::{EntityType, MobEffect, SoundEvent};
use tuff_vanilla_data::{Block, BlockState};

use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{LookAtPlayer, RandomLookAround, RandomStroll};
use super::target::{HurtByTarget, NearestAttackableEntity, NearestAttackablePlayer, TargetView};
use super::{MobCore, MobSounds, Species};
use tuff_core::world::entity::{Culprit, TickContext};
use tuff_core::world::projectile::{self, Launch, Owner, Shot, shot_velocity};
use tuff_world_model::BlockPos;

const BREEZE_XP: i32 = 10;
/// Squared: it only fires within sixteen blocks.
const ATTACK_RANGE_SQ: f64 = 256.0;
const CHARGE_TICKS: i32 = 15;
const RECOVER_TICKS: i32 = 4;
const COOLDOWN_TICKS: i32 = 10;
const SHOOT_DURATION: i32 = CHARGE_TICKS + 1 + RECOVER_TICKS;
const JUMP_DURATION: i32 = 200;
const INHALING_TICKS: i32 = 10;
const JUMP_COOLDOWN_TICKS: i32 = 10;
const JUMP_COOLDOWN_HURT_TICKS: i32 = 2;
const JUMP_SHOOT_TICKS: i32 = 100;
const SLIDE_SHOOT_TICKS: i32 = 60;
const STUCK_SHOOT_TICKS: i32 = 60;
const HURT_MEMORY_TICKS: u64 = 100;
const SLIDE_SPEED: f64 = 0.6;
const SLIDE_MIN_TICKS: i32 = 20;
const SLIDE_MAX_TICKS: i32 = 40;
const INNER_RADIUS: f64 = 4.0;
const INNER_HEIGHT: f64 = 10.0;
const MIDDLE_RADIUS_MIN: f64 = 4.0;
const MIDDLE_RADIUS_MAX: f64 = 8.0;
const LINE_OF_SIGHT_RANGE: f64 = 50.0;
const SURFACE_SEARCH: f64 = 10.0;
const REQUIRED_AIR_ABOVE: i32 = 4;
const JUMP_ANGLES: [i32; 5] = [40, 55, 60, 75, 80];
const MAX_JUMP_VELOCITY_PER_RANGE: f32 = 0.058_333_334;
const JUMP_SCALE: f64 = 0.949_999_988_079_071;
const SHOT_POWER: f32 = 0.7;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Ready,
    Shoot { ticks: i32 },
    Jump { ticks: i32 },
    Slide { ticks: i32, limit: i32 },
}

/// The breeze's fight: it shoots when it has just landed or slid, leaps to
/// a spot behind its target, slides around the rings of the target, and
/// shoots when it cannot move. The memories of the reference's brain are
/// the timers below, and its behaviors the phases, tried in its order.
#[derive(Debug)]
struct BreezeFight {
    phase: Phase,
    shoot_memory: i32,
    shoot_cooldown: i32,
    charging: i32,
    recovering: i32,
    jump_cooldown: i32,
    inhaling: i32,
    jump_target: Option<BlockPos>,
    leaving_water: bool,
}

impl Default for BreezeFight {
    fn default() -> Self {
        Self {
            phase: Phase::Ready,
            shoot_memory: 0,
            shoot_cooldown: 0,
            charging: 0,
            recovering: 0,
            jump_cooldown: 0,
            inhaling: 0,
            jump_target: None,
            leaving_water: false,
        }
    }
}

impl BreezeFight {
    fn age_memories(&mut self) {
        for timer in [
            &mut self.shoot_memory,
            &mut self.shoot_cooldown,
            &mut self.charging,
            &mut self.recovering,
            &mut self.jump_cooldown,
            &mut self.inhaling,
        ] {
            *timer = (*timer - 1).max(0);
        }
    }

    fn tick_running(&mut self, mob: &mut MobCore, target: &TargetView, cx: &mut TickContext) {
        match self.phase {
            Phase::Ready => {}
            Phase::Shoot { ticks } => {
                if ticks > SHOOT_DURATION || self.shoot_memory == 0 {
                    self.stop_shoot(mob);
                    return;
                }
                self.phase = Phase::Shoot { ticks: ticks + 1 };
                mob.look.set_look_at(target.position, 30.0, 30.0);
                if self.charging > 0 || self.recovering > 0 {
                    return;
                }
                self.recovering = RECOVER_TICKS;
                shoot(mob, target, cx);
            }
            Phase::Jump { ticks } => {
                if mob.pose == Some(Pose::Standing) || mob.pose.is_none() || self.jump_cooldown > 0
                {
                    self.stop_jump(mob);
                    return;
                }
                if ticks > JUMP_DURATION {
                    self.stop_jump(mob);
                    return;
                }
                self.phase = Phase::Jump { ticks: ticks + 1 };
                self.tick_jump(mob, cx);
            }
            Phase::Slide { ticks, limit } => {
                if ticks >= limit || mob.navigation.is_done() {
                    self.stop_slide(mob);
                    return;
                }
                self.phase = Phase::Slide {
                    ticks: ticks + 1,
                    limit,
                };
            }
        }
    }

    fn stop_shoot(&mut self, mob: &mut MobCore) {
        if mob.pose == Some(Pose::Shooting) {
            mob.set_pose(Some(Pose::Standing));
        }
        self.shoot_cooldown = COOLDOWN_TICKS;
        self.shoot_memory = 0;
        self.phase = Phase::Ready;
    }

    fn stop_jump(&mut self, mob: &mut MobCore) {
        if matches!(mob.pose, Some(Pose::LongJumping | Pose::Inhaling)) {
            mob.set_pose(Some(Pose::Standing));
        }
        mob.discard_friction = false;
        self.jump_target = None;
        self.inhaling = 0;
        self.leaving_water = false;
        self.phase = Phase::Ready;
    }

    fn stop_slide(&mut self, mob: &mut MobCore) {
        mob.navigation.stop();
        mob.set_pose(Some(Pose::Standing));
        if mob.target.is_some() {
            self.shoot_memory = SLIDE_SHOOT_TICKS;
        }
        self.phase = Phase::Ready;
    }

    fn tick_jump(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let in_water = mob.body.in_water;
        if !in_water && self.leaving_water {
            self.leaving_water = false;
        }
        let inhaling_done = self.inhaling == 0 && mob.pose == Some(Pose::Inhaling);
        if !inhaling_done {
            let jumping = mob.pose == Some(Pose::LongJumping);
            let landed = mob.base.on_ground || (in_water && !self.leaving_water);
            if jumping && landed {
                mob.sound(SoundEvent::EntityBreezeLand, 1.0, 1.0, cx);
                mob.set_pose(Some(Pose::Standing));
                mob.discard_friction = false;
                let hurt = mob
                    .last_damage
                    .as_ref()
                    .is_some_and(|(_, tick)| cx.tick.saturating_sub(*tick) < HURT_MEMORY_TICKS);
                self.jump_cooldown = if hurt {
                    JUMP_COOLDOWN_HURT_TICKS
                } else {
                    JUMP_COOLDOWN_TICKS
                };
                self.shoot_memory = JUMP_SHOOT_TICKS;
            }
            return;
        }
        let vector = self.jump_target.and_then(|target| {
            let to = [
                f64::from(target.x) + 0.5,
                f64::from(target.y),
                f64::from(target.z) + 0.5,
            ];
            optimal_jump_vector(mob, to, cx)
        });
        let Some(vector) = vector else {
            mob.set_pose(Some(Pose::Standing));
            return;
        };
        if in_water {
            self.leaving_water = true;
        }
        mob.sound(SoundEvent::EntityBreezeJump, 1.0, 1.0, cx);
        mob.set_pose(Some(Pose::LongJumping));
        mob.discard_friction = true;
        mob.base.velocity = vector;
    }

    fn try_shoot(&mut self, mob: &mut MobCore, target: &TargetView, cx: &mut TickContext) -> bool {
        if mob.pose.is_some_and(|p| p != Pose::Standing) || self.jump_target.is_some() {
            return false;
        }
        if mob.distance_sq(target.position) >= ATTACK_RANGE_SQ {
            self.shoot_memory = 0;
            return false;
        }
        if self.shoot_memory == 0 || self.shoot_cooldown > 0 || self.charging > 0 {
            return false;
        }
        self.recovering = 0;
        mob.set_pose(Some(Pose::Shooting));
        self.charging = CHARGE_TICKS;
        mob.sound(SoundEvent::EntityBreezeInhale, 1.0, 1.0, cx);
        self.phase = Phase::Shoot { ticks: 0 };
        true
    }

    fn try_long_jump(
        &mut self,
        mob: &mut MobCore,
        target: &TargetView,
        cx: &mut TickContext,
    ) -> bool {
        if self.jump_cooldown > 0 || self.shoot_memory > 0 {
            return false;
        }
        let in_fluid = mob.body.in_water;
        if (!mob.base.on_ground && !in_fluid)
            || mob.body.eyes_in_water
            || mob.body.lava_height > 0.0
        {
            return false;
        }
        if self.jump_target.is_none() && !self.pick_jump_target(mob, target, cx) {
            return false;
        }
        self.inhaling = INHALING_TICKS;
        mob.set_pose(Some(Pose::Inhaling));
        mob.sound(SoundEvent::EntityBreezeCharge, 1.0, 1.0, cx);
        if let Some(spot) = self.jump_target {
            mob.look_at_point([
                f64::from(spot.x) + 0.5,
                f64::from(spot.y) + 0.5,
                f64::from(spot.z) + 0.5,
            ]);
        }
        self.phase = Phase::Jump { ticks: 0 };
        true
    }

    fn pick_jump_target(
        &mut self,
        mob: &mut MobCore,
        target: &TargetView,
        cx: &mut TickContext,
    ) -> bool {
        if mob.distance_sq(target.position) >= mob.follow_range().powi(2) {
            mob.target = None;
            return false;
        }
        if mob.distance_sq(target.position).sqrt() - 4.0 <= 0.0 || !can_jump_from_here(mob, cx) {
            return false;
        }
        let head = head_yaw(cx, target.id);
        let behind = random_point_behind(target.position, head, cx.rng);
        let Some(spot) = snap_to_surface(behind, cx) else {
            return false;
        };
        let below = (cx.block)(BlockPos::new(spot.x, spot.y - 1, spot.z));
        if below.is_some_and(dangerous_block) {
            return false;
        }
        let center = [
            f64::from(spot.x) + 0.5,
            f64::from(spot.y) + 0.5,
            f64::from(spot.z) + 0.5,
        ];
        let raised = [center[0], center[1] + 4.0, center[2]];
        if !line_of_sight(mob, center, cx) && !line_of_sight(mob, raised, cx) {
            return false;
        }
        self.jump_target = Some(spot);
        true
    }

    fn try_shoot_when_stuck(&mut self, mob: &MobCore) {
        let stuck = mob.base.vehicle.is_some()
            || mob.body.in_water
            || mob.effects.has(MobEffect::Levitation);
        if stuck && self.inhaling == 0 && self.jump_target.is_none() && self.shoot_memory == 0 {
            self.shoot_memory = STUCK_SHOOT_TICKS;
        }
    }

    fn try_slide(&mut self, mob: &mut MobCore, target: &TargetView, cx: &mut TickContext) -> bool {
        if self.jump_cooldown > 0
            || self.shoot_memory > 0
            || !mob.base.on_ground
            || mob.body.in_water
            || mob.pose.is_some_and(|p| p != Pose::Standing)
        {
            return false;
        }
        let spot = self.slide_spot(mob, target, cx);
        if !mob.move_to_position(spot, SLIDE_SPEED, cx) {
            return false;
        }
        mob.sound(SoundEvent::EntityBreezeSlide, 1.0, 1.0, cx);
        mob.set_pose(Some(Pose::Sliding));
        let limit = cx.rng.gen_range(SLIDE_MIN_TICKS..=SLIDE_MAX_TICKS);
        self.phase = Phase::Slide { ticks: 0, limit };
        true
    }

    fn slide_spot(&self, mob: &mut MobCore, target: &TargetView, cx: &mut TickContext) -> [f64; 3] {
        if within_inner_circle(mob, target.position)
            && let Some(away) = super::random_pos::pos_away(mob, 5, 5, target.position, cx)
            && line_of_sight(mob, away, cx)
            && tuff_core::world::distance_sq(target.position, away)
                > tuff_core::world::distance_sq(target.position, mob.base.position)
        {
            return away;
        }
        if cx.rng.r#gen::<bool>() {
            random_point_behind(target.position, head_yaw(cx, target.id), cx.rng)
        } else {
            middle_circle_point(mob.base.position, target.position, cx.rng)
        }
    }
}

impl Goal for BreezeFight {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.target_view(cx).is_some_and(|t| t.alive)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.can_use(mob, cx)
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        *self = Self::default();
        mob.set_pose(None);
        mob.discard_friction = false;
        mob.navigation.stop();
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        self.age_memories();
        self.tick_running(mob, &target, cx);
        if self.phase != Phase::Ready {
            return;
        }
        if self.try_shoot(mob, &target, cx) || self.try_long_jump(mob, &target, cx) {
            return;
        }
        self.try_shoot_when_stuck(mob);
        self.try_slide(mob, &target, cx);
    }
}

fn within_inner_circle(mob: &MobCore, target: [f64; 3]) -> bool {
    let here = mob.base.position;
    let center = [
        here[0].floor() + 0.5,
        here[1].floor() + 0.5,
        here[2].floor() + 0.5,
    ];
    let (dx, dz) = (target[0] - center[0], target[2] - center[2]);
    dx * dx + dz * dz < INNER_RADIUS * INNER_RADIUS && (target[1] - center[1]).abs() < INNER_HEIGHT
}

fn head_yaw(cx: &TickContext, id: i32) -> f32 {
    cx.players
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.yaw)
        .or_else(|| cx.entities.iter().find(|e| e.id == id).map(|e| e.yaw))
        .unwrap_or(0.0)
}

fn gaussian(rng: &mut StdRng) -> f32 {
    let u: f32 = 1.0 - rng.r#gen::<f32>();
    let v: f32 = rng.r#gen();
    (-2.0 * u.ln()).sqrt() * (std::f32::consts::TAU * v).cos()
}

fn random_point_behind(target: [f64; 3], head_yaw: f32, rng: &mut StdRng) -> [f64; 3] {
    let angle = (head_yaw + 180.0 + gaussian(rng) * 90.0 / 2.0).to_radians();
    let reach = f64::from(4.0 + rng.r#gen::<f32>() * 4.0);
    [
        target[0] - f64::from(angle.sin()) * reach,
        target[1],
        target[2] + f64::from(angle.cos()) * reach,
    ]
}

fn middle_circle_point(from: [f64; 3], target: [f64; 3], rng: &mut StdRng) -> [f64; 3] {
    let direction = [
        target[0] - from[0],
        target[1] - from[1],
        target[2] - from[2],
    ];
    let length = direction.iter().map(|c| c * c).sum::<f64>().sqrt();
    if length < 1.0e-6 {
        return from;
    }
    let distance =
        length - (MIDDLE_RADIUS_MAX + rng.r#gen::<f64>() * (MIDDLE_RADIUS_MIN - MIDDLE_RADIUS_MAX));
    let scale = distance / length;
    [
        from[0] + direction[0] * scale,
        from[1] + direction[1] * scale,
        from[2] + direction[2] * scale,
    ]
}

fn level_of<'a>(cx: &'a TickContext) -> impl Fn(BlockPos) -> BlockState + 'a {
    move |pos| (cx.block)(pos).unwrap_or(Block::Stone.default_state())
}

fn line_of_sight(mob: &MobCore, to: [f64; 3], cx: &TickContext) -> bool {
    let from = mob.base.position;
    let range = mob.follow_range().max(LINE_OF_SIGHT_RANGE);
    tuff_core::world::distance_sq(from, to) <= range * range
        && tuff_core::world::raycast::cast_colliders(from, to, &level_of(cx)).is_none()
}

fn snap_to_surface(point: [f64; 3], cx: &TickContext) -> Option<BlockPos> {
    let level = level_of(cx);
    for dy in [-SURFACE_SEARCH, SURFACE_SEARCH] {
        let end = [point[0], point[1] + dy, point[2]];
        if let Some(hit) = tuff_core::world::raycast::cast_colliders(point, end, &level) {
            let at = BlockPos::new(
                hit.location[0].floor() as i32,
                hit.location[1].floor() as i32,
                hit.location[2].floor() as i32,
            );
            return Some(BlockPos::new(at.x, at.y + 1, at.z));
        }
    }
    None
}

fn can_jump_from_here(mob: &MobCore, cx: &TickContext) -> bool {
    let here = BlockPos::new(
        mob.base.position[0].floor() as i32,
        mob.base.position[1].floor() as i32,
        mob.base.position[2].floor() as i32,
    );
    if (cx.block)(here).is_some_and(|s| s.block() == Block::HoneyBlock) {
        return false;
    }
    (1..=REQUIRED_AIR_ABOVE).all(|i| {
        (cx.block)(BlockPos::new(here.x, here.y + i, here.z)).is_none_or(|state| {
            tuff_core_is_air(state)
                || tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::fluid::key::WATER,
                    state.fluid_state().fluid(),
                )
        })
    })
}

fn tuff_core_is_air(state: BlockState) -> bool {
    tuff_world_model::chunk::is_air(state)
}

fn dangerous_block(state: BlockState) -> bool {
    matches!(
        state.block(),
        Block::Fire
            | Block::SoulFire
            | Block::Lava
            | Block::MagmaBlock
            | Block::WitherRose
            | Block::SweetBerryBush
            | Block::Cactus
            | Block::PowderSnow
    ) || matches!(state.block(), Block::Campfire | Block::SoulCampfire)
        && state.get("lit") == Some("true")
}

fn optimal_jump_vector(mob: &MobCore, to: [f64; 3], cx: &mut TickContext) -> Option<[f64; 3]> {
    let mut angles = JUMP_ANGLES;
    angles.shuffle(cx.rng);
    let max_velocity = f64::from(MAX_JUMP_VELOCITY_PER_RANGE * mob.follow_range() as f32);
    let gravity = mob.effective_gravity();
    for angle in angles {
        let Some(vector) = jump_vector(mob.base.position, to, gravity, max_velocity, angle) else {
            continue;
        };
        let boost = mob
            .effects
            .amplifier(MobEffect::JumpBoost)
            .map_or(0.0, |a| 0.1 * (f64::from(a) + 1.0));
        if boost == 0.0 {
            return Some(vector);
        }
        let length = vector.iter().map(|c| c * c).sum::<f64>().sqrt();
        let lift = vector[1] / length * boost;
        return Some([vector[0], vector[1] + lift, vector[2]]);
    }
    None
}

/// The launch velocity that lands a body under `gravity` on `to`, leaving
/// at `angle` degrees above the horizontal, if its speed stays within
/// `max_velocity`. The aim is half a block short of the target.
fn jump_vector(
    from: [f64; 3],
    to: [f64; 3],
    gravity: f64,
    max_velocity: f64,
    angle: i32,
) -> Option<[f64; 3]> {
    let (px, pz) = (to[0] - from[0], to[2] - from[2]);
    let plane = (px * px + pz * pz).sqrt();
    let (aim_x, aim_z) = if plane > 0.0 {
        (to[0] - px / plane * 0.5, to[2] - pz / plane * 0.5)
    } else {
        (to[0], to[2])
    };
    let direction = [aim_x - from[0], to[1] - from[1], aim_z - from[2]];
    let radians = f64::from(angle as f32 * std::f32::consts::PI / 180.0);
    let heading = direction[2].atan2(direction[0]);
    let r2 = direction[0] * direction[0] + direction[2] * direction[2];
    let r = r2.sqrt();
    let y = direction[1];
    let v0_squared = r2 * gravity
        / (r * f64::from((2.0 * radians as f32).sin()) - 2.0 * y * radians.cos().powi(2));
    if v0_squared < 0.0 || v0_squared.is_nan() {
        return None;
    }
    let v0 = v0_squared.sqrt();
    if v0 > max_velocity {
        return None;
    }
    let horizontal = v0 * radians.cos();
    let vertical = v0 * radians.sin();
    Some([
        horizontal * heading.cos() * JUMP_SCALE,
        vertical * JUMP_SCALE,
        horizontal * heading.sin() * JUMP_SCALE,
    ])
}

fn shoot(mob: &mut MobCore, target: &TargetView, cx: &mut TickContext) {
    let [x, y, z] = mob.base.position;
    let start = [x, y + f64::from(mob.size.height) * 0.5 + 0.3, z];
    let aim_y = target.bounds[1] + (target.bounds[4] - target.bounds[1]) * 0.3;
    let direction = [
        target.position[0] - x,
        aim_y - start[1],
        target.position[2] - z,
    ];
    let uncertainty = 5.0 - f32::from(cx.shared.difficulty.borrow().id()) * 4.0;
    let velocity = shot_velocity(direction, SHOT_POWER, uncertainty.max(0.0), cx.rng);
    let owner: Owner = Owner::of(Culprit::entity(&mob.base), None);
    let id = cx.shared.next_entity_id();
    let shot = Shot::WindCharge {
        kind: EntityType::BreezeWindCharge,
        velocity,
    };
    mob.sound(SoundEvent::EntityBreezeShoot, 1.5, 1.0, cx);
    projectile::launch(
        Launch {
            id,
            position: start,
            owner,
            shot,
        },
        cx,
    );
}

#[derive(Debug, Default)]
pub struct Breeze;

impl Species for Breeze {
    fn create(_rng: &mut StdRng) -> Self {
        Self
    }

    fn debug_value(
        &self,
        core: &MobCore,
        feed: tuff_vanilla_data::registry::DebugSubscription,
        _tick: u64,
    ) -> Option<tuff_core::protocol::play::DebugValue> {
        (feed == tuff_vanilla_data::registry::DebugSubscription::Breezes).then(|| {
            tuff_core::protocol::play::DebugValue::Breeze {
                attack_target: core.target,
                jump_target: core
                    .goals
                    .find::<BreezeFight>()
                    .and_then(|fight| fight.jump_target),
            }
        })
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        core.xp_reward = BREEZE_XP;
        core.set_path_malus(tuff_vanilla_data::entity::PathType::Trapdoor, -1.0);
        core.set_path_malus(tuff_vanilla_data::entity::PathType::Fire, -1.0);
        let mut goals = GoalSelector::default();
        goals.add(1, HurtByTarget::new(&[]));
        goals.add(2, NearestAttackablePlayer::with_interval(10, true, None));
        goals.add(
            2,
            NearestAttackableEntity::new(&[EntityType::IronGolem], true),
        );
        goals.add(3, BreezeFight::default());
        goals.add(7, RandomStroll::water_avoiding(0.6));
        goals.add(8, LookAtPlayer::new(8.0));
        goals.add(8, RandomLookAround::default());
        core.goals = goals;
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(if core.base.on_ground {
                SoundEvent::EntityBreezeIdleGround
            } else {
                SoundEvent::EntityBreezeIdleAir
            }),
            hurt: Some(SoundEvent::EntityBreezeHurt),
            death: Some(SoundEvent::EntityBreezeDeath),
            step: None,
            volume: 1.0,
        }
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Breeze, super::load::<Breeze>);
    registry.register_spawn_rule(EntityType::Breeze, super::spawning::monster_rule);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view_at(x: f64) -> TargetView {
        TargetView {
            id: 7,
            uuid: 7,
            kind: EntityType::Player,
            position: [x, 64.0, 0.5],
            bounds: [x - 0.3, 64.0, 0.2, x + 0.3, 65.8, 0.8],
            eye: [x, 65.6, 0.5],
            alive: true,
            creative: false,
            invulnerable: false,
            in_water: false,
            seen_as_enemy: true,
            owner: None,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_breeze_turns_projectiles_back_but_not_wind_charges() {
        use tuff_core::world::projectile::{Target, deflect_off};
        crate::tests::with_context(|core, cx| {
            let target = |kind| Target {
                id: 9,
                kind,
                position: [4.0, 64.0, 0.5],
                bounds: [3.7, 64.0, 0.2, 4.3, 65.8, 0.8],
                location: [3.7, 64.5, 0.5],
            };
            let mut arrow = core.base.clone();
            arrow.kind = EntityType::Arrow;
            arrow.position = [3.0, 64.5, 0.5];
            arrow.velocity = [2.0, 0.0, 0.0];
            let before = cx.effects.len();
            assert!(deflect_off(&mut arrow, &target(EntityType::Breeze), cx));
            assert_eq!(arrow.velocity, [-1.0, 0.0, 0.0], "back at half speed");
            assert_eq!(cx.effects.len(), before + 1, "with the deflect sound");
            assert!(
                deflect_off(&mut arrow, &target(EntityType::Breeze), cx),
                "still no hit while leaving"
            );
            assert_eq!(arrow.velocity, [-1.0, 0.0, 0.0]);
            assert!(!deflect_off(&mut arrow, &target(EntityType::Zombie), cx));
            arrow.kind = EntityType::WindCharge;
            arrow.velocity = [2.0, 0.0, 0.0];
            assert!(!deflect_off(&mut arrow, &target(EntityType::Breeze), cx));
            assert_eq!(arrow.velocity, [2.0, 0.0, 0.0]);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_jump_vector_lands_on_the_aim_point_within_the_speed_limit() {
        let from = [0.5, 64.0, 0.5];
        let to = [10.5, 65.0, 0.5];
        let vector = jump_vector(from, to, 0.08, 1.4, 55).unwrap();
        let (vx, vy) = (vector[0] / JUMP_SCALE, vector[1] / JUMP_SCALE);
        let time = 9.5 / vx;
        let height = vy * time - 0.08 * time * time / 2.0;
        assert!((height - 1.0).abs() < 1.0e-6, "{height}");
        assert!(vector[2].abs() < 1.0e-9);
        assert!(jump_vector(from, [60.5, 65.0, 0.5], 0.08, 1.4, 55).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_point_behind_a_target_lies_four_to_eight_blocks_opposite_its_gaze() {
        let mut rng = <StdRng as rand::SeedableRng>::seed_from_u64(1);
        for _ in 0..200 {
            let point = random_point_behind([0.0, 64.0, 0.0], 0.0, &mut rng);
            let distance = (point[0] * point[0] + point[2] * point[2]).sqrt();
            assert!((4.0..=8.0).contains(&distance), "{distance}");
        }
        let straight = random_point_behind([0.0, 64.0, 0.0], 0.0, &mut rng);
        assert!(straight[2] > -1.0e-9 || straight[0].abs() > 0.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn it_shoots_only_with_the_shoot_memory_and_a_target_in_range() {
        crate::tests::with_context(|core, cx| {
            let mut fight = BreezeFight::default();
            let near = view_at(core.base.position[0] + 8.0);
            assert!(!fight.try_shoot(core, &near, cx));
            fight.shoot_memory = SLIDE_SHOOT_TICKS;
            assert!(fight.try_shoot(core, &near, cx));
            assert_eq!(core.pose, Some(Pose::Shooting));
            assert_eq!(fight.phase, Phase::Shoot { ticks: 0 });
            let mut far_fight = BreezeFight {
                shoot_memory: SLIDE_SHOOT_TICKS,
                ..BreezeFight::default()
            };
            core.set_pose(None);
            let far = view_at(core.base.position[0] + 40.0);
            assert!(!far_fight.try_shoot(core, &far, cx));
            assert_eq!(far_fight.shoot_memory, 0);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_stuck_breeze_shoots_and_a_landed_one_cools_down_before_jumping_again() {
        crate::tests::with_context(|core, _cx| {
            let mut fight = BreezeFight::default();
            core.body.in_water = true;
            fight.try_shoot_when_stuck(core);
            assert_eq!(fight.shoot_memory, STUCK_SHOOT_TICKS);
            core.body.in_water = false;
            let mut dry = BreezeFight::default();
            dry.try_shoot_when_stuck(core);
            assert_eq!(dry.shoot_memory, 0);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn fire_and_thorns_are_dangerous_landings() {
        assert!(dangerous_block(Block::Fire.default_state()));
        assert!(dangerous_block(Block::Cactus.default_state()));
        assert!(!dangerous_block(Block::Stone.default_state()));
    }
}
