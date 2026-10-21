//! The goat and frog long jump: after a cooldown a mob that stands still
//! picks a landing spot it cannot simply walk to, crouches for a while and
//! leaps along a ballistic arc, landing with a thud.

use rand::Rng;
use rand::seq::SliceRandom;
use tuff_vanilla_data::entity::Pose;
use tuff_vanilla_data::registry::{Attribute, MobEffect, SoundEvent};
use tuff_vanilla_data::{Block, BlockState};

use super::MobCore;
use super::goal::{Flags, Goal};
use super::path::static_path_type;
use super::physics::{self, block_pos};
use tuff_core::world::entity::{DebugPath, TickContext};
use tuff_world_model::BlockPos;

const FIND_JUMP_TRIES: i32 = 20;
const PREPARE_TICKS: u64 = 40;
const MID_JUMP_TIMEOUT: u64 = 100;
const ANGLES: [i32; 4] = [65, 70, 75, 80];
const JUMP_SCALE: f64 = 0.949_999_988_079_071;
pub const MAX_VELOCITY_MULTIPLIER: f32 = 3.571_428_8;
const LANDING_SPEED_KEPT: f64 = 0.100_000_001_490_116_12;
const LANDING_VOLUME: f32 = 2.0;

pub type Landing = fn(&MobCore, BlockPos, &mut TickContext) -> bool;

#[derive(Debug, Clone, Copy)]
pub struct Preference {
    pub block: fn(BlockState) -> bool,
    pub chance: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct LongJumpSpec {
    pub cooldown: (i32, i32),
    pub height: i32,
    pub width: i32,
    pub jump_sound: SoundEvent,
    pub screaming_jump_sound: SoundEvent,
    pub landing_sound: SoundEvent,
    pub dimension_scale: f32,
    pub preference: Option<Preference>,
    pub landing: Landing,
    pub blocked: fn(&MobCore) -> bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    Searching,
    Preparing { since: u64 },
    Jumping { since: u64, left_ground: bool },
}

#[derive(Debug)]
pub struct LongJump {
    spec: LongJumpSpec,
    pub screaming: bool,
    phase: Phase,
    next_allowed: u64,
    initial_position: [f64; 3],
    tries: i32,
    candidates: Vec<(BlockPos, i32)>,
    not_preferred: Vec<(BlockPos, i32)>,
    wants_preferred: bool,
    chosen: Option<[f64; 3]>,
}

impl LongJump {
    pub fn new(spec: LongJumpSpec) -> Self {
        Self {
            spec,
            screaming: false,
            phase: Phase::Idle,
            next_allowed: 0,
            initial_position: [0.0; 3],
            tries: 0,
            candidates: Vec::new(),
            not_preferred: Vec::new(),
            wants_preferred: false,
            chosen: None,
        }
    }

    fn sample_cooldown(&self, cx: &mut TickContext) -> i32 {
        cx.rng
            .gen_range(self.spec.cooldown.0..=self.spec.cooldown.1)
    }

    fn pick_weighted(&mut self, cx: &mut TickContext) -> Option<(BlockPos, i32)> {
        let total: i64 = self.candidates.iter().map(|c| i64::from(c.1.max(1))).sum();
        if total <= 0 {
            return None;
        }
        let mut roll = cx.rng.gen_range(0..total);
        let index = self.candidates.iter().position(|c| {
            roll -= i64::from(c.1.max(1));
            roll < 0
        })?;
        Some(self.candidates.swap_remove(index))
    }

    fn next_candidate(&mut self, cx: &mut TickContext) -> Option<BlockPos> {
        let Some(preference) = self.spec.preference.filter(|_| self.wants_preferred) else {
            return self.pick_weighted(cx).map(|c| c.0);
        };
        while !self.candidates.is_empty() {
            let picked = self.pick_weighted(cx)?;
            let below = BlockPos::new(picked.0.x, picked.0.y - 1, picked.0.z);
            if (cx.block)(below).is_some_and(preference.block) {
                return Some(picked.0);
            }
            self.not_preferred.push(picked);
        }
        if self.not_preferred.is_empty() {
            None
        } else {
            Some(self.not_preferred.remove(0).0)
        }
    }

    fn pick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let here = block_pos(mob.base.position);
        while !self.candidates.is_empty() {
            let Some(target) = self.next_candidate(cx) else {
                continue;
            };
            if target.x == here.x && target.z == here.z {
                continue;
            }
            if !(self.spec.landing)(mob, target, cx) {
                continue;
            }
            let center = [
                f64::from(target.x) + 0.5,
                f64::from(target.y) + 0.5,
                f64::from(target.z) + 0.5,
            ];
            let Some(vector) = self.optimal_vector(mob, center, cx) else {
                continue;
            };
            if !matches!(mob.debug_path(target, cx), DebugPath::Made) {
                self.chosen = Some(vector);
                mob.look_at_point(center);
                return;
            }
            mob.look_at_point(center);
        }
    }

    fn optimal_vector(
        &self,
        mob: &MobCore,
        target: [f64; 3],
        cx: &mut TickContext,
    ) -> Option<[f64; 3]> {
        let mut angles = ANGLES;
        angles.shuffle(cx.rng);
        let max_velocity =
            mob.attribute(Attribute::JumpStrength) * f64::from(MAX_VELOCITY_MULTIPLIER);
        let scale = self.spec.dimension_scale;
        angles.into_iter().find_map(|angle| {
            jump_vector(
                mob,
                target,
                max_velocity,
                angle,
                [mob.size.width * scale, mob.size.height * scale],
                cx,
            )
        })
    }

    fn finish(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if matches!(self.phase, Phase::Jumping { .. }) {
            if mob.base.on_ground {
                mob.base.velocity[0] *= LANDING_SPEED_KEPT;
                mob.base.velocity[2] *= LANDING_SPEED_KEPT;
                mob.sound(self.spec.landing_sound, LANDING_VOLUME, 1.0, cx);
            }
            mob.discard_friction = false;
            mob.set_pose(Some(Pose::Standing));
            if self.spec.dimension_scale != 1.0 {
                mob.size_scale /= self.spec.dimension_scale;
                mob.refresh_size();
            }
            self.next_allowed = cx.tick + self.sample_cooldown(cx) as u64;
        } else if self.phase != Phase::Idle {
            self.next_allowed = cx.tick + (self.sample_cooldown(cx) / 2) as u64;
        }
        self.phase = Phase::Idle;
        self.chosen = None;
        self.candidates.clear();
        self.not_preferred.clear();
    }

    fn launch(&mut self, mob: &mut MobCore, vector: [f64; 3], cx: &mut TickContext) {
        mob.base.yaw = mob.body_yaw;
        mob.discard_friction = true;
        let length = vector.iter().map(|c| c * c).sum::<f64>().sqrt();
        let boost = mob
            .effects
            .amplifier(MobEffect::JumpBoost)
            .map_or(0.0, |a| 0.1 * (f64::from(a) + 1.0));
        let factor = if length > 0.0 {
            (length + boost) / length
        } else {
            1.0
        };
        mob.base.velocity = vector.map(|c| c * factor);
        mob.set_pose(Some(Pose::LongJumping));
        if self.spec.dimension_scale != 1.0 {
            mob.size_scale *= self.spec.dimension_scale;
            mob.refresh_size();
        }
        let sound = if self.screaming {
            self.spec.screaming_jump_sound
        } else {
            self.spec.jump_sound
        };
        mob.sound(sound, 1.0, 1.0, cx);
        self.phase = Phase::Jumping {
            since: cx.tick,
            left_ground: false,
        };
    }
}

impl Goal for LongJump {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK | Flags::JUMP
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn interruptable(&self) -> bool {
        !matches!(self.phase, Phase::Jumping { .. })
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if self.next_allowed == 0 {
            self.next_allowed = cx.tick + self.sample_cooldown(cx) as u64;
        }
        if cx.tick < self.next_allowed
            || mob.target.is_some()
            || mob.base.vehicle.is_some()
            || (self.spec.blocked)(mob)
            || !mob.navigation.is_done()
        {
            return false;
        }
        let here = block_pos(mob.base.position);
        let honey = (cx.block)(here).is_some_and(|s| s.block() == Block::HoneyBlock);
        let ready =
            mob.base.on_ground && !mob.body.in_water && mob.body.lava_height <= 0.0 && !honey;
        if !ready {
            self.next_allowed = cx.tick + (self.sample_cooldown(cx) / 2) as u64;
        }
        ready
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        match self.phase {
            Phase::Idle => false,
            Phase::Searching | Phase::Preparing { .. } => {
                self.initial_position == mob.base.position
                    && self.tries > 0
                    && !mob.body.in_water
                    && (self.chosen.is_some() || !self.candidates.is_empty())
            }
            Phase::Jumping { since, left_ground } => {
                cx.tick < since + MID_JUMP_TIMEOUT && !(left_ground && mob.base.on_ground)
            }
        }
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.phase = Phase::Searching;
        self.chosen = None;
        self.tries = FIND_JUMP_TRIES;
        self.initial_position = mob.base.position;
        self.not_preferred.clear();
        self.wants_preferred = self
            .spec
            .preference
            .is_some_and(|p| cx.rng.r#gen::<f32>() < p.chance);
        let here = block_pos(mob.base.position);
        let (w, h) = (self.spec.width, self.spec.height);
        self.candidates.clear();
        for x in here.x - w..=here.x + w {
            for y in here.y - h..=here.y + h {
                for z in here.z - w..=here.z + w {
                    let (dx, dy, dz) = (x - here.x, y - here.y, z - here.z);
                    if dx == 0 && dy == 0 && dz == 0 {
                        continue;
                    }
                    let weight = f64::from(dx * dx + dy * dy + dz * dz).ceil() as i32;
                    self.candidates.push((BlockPos::new(x, y, z), weight));
                }
            }
        }
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        match self.phase {
            Phase::Searching => {
                self.tries -= 1;
                self.pick(mob, cx);
                if self.chosen.is_some() {
                    self.phase = Phase::Preparing { since: cx.tick };
                }
            }
            Phase::Preparing { since } => {
                if cx.tick - since >= PREPARE_TICKS
                    && let Some(vector) = self.chosen
                {
                    self.launch(mob, vector, cx);
                }
            }
            Phase::Jumping { since, left_ground } => {
                if !left_ground && !mob.base.on_ground {
                    self.phase = Phase::Jumping {
                        since,
                        left_ground: true,
                    };
                }
                mob.discard_friction = true;
            }
            Phase::Idle => {}
        }
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.finish(mob, cx);
    }
}

/// A landing spot that is firm ground it would happily walk on.
pub fn default_landing(mob: &MobCore, target: BlockPos, cx: &mut TickContext) -> bool {
    let below = BlockPos::new(target.x, target.y - 1, target.z);
    (cx.block)(below).is_some_and(BlockState::is_solid_render)
        && mob.path_malus(static_path_type(target, cx.block, cx.min_y)) == 0.0
}

fn jump_vector(
    mob: &MobCore,
    target: [f64; 3],
    max_velocity: f64,
    angle: i32,
    dimensions: [f32; 2],
    cx: &TickContext,
) -> Option<[f64; 3]> {
    let from = mob.base.position;
    let (px, pz) = (target[0] - from[0], target[2] - from[2]);
    let plane = (px * px + pz * pz).sqrt();
    let (aim_x, aim_z) = if plane > 0.0 {
        (target[0] - px / plane * 0.5, target[2] - pz / plane * 0.5)
    } else {
        (target[0], target[2])
    };
    let direction = [aim_x - from[0], target[1] - from[1], aim_z - from[2]];
    let radians = f64::from(angle as f32 * std::f32::consts::PI / 180.0);
    let heading = direction[2].atan2(direction[0]);
    let r2 = direction[0] * direction[0] + direction[2] * direction[2];
    let r = r2.sqrt();
    let y = direction[1];
    let gravity = mob.effective_gravity();
    let sin_2 = f64::from((2.0 * radians as f32).sin());
    let cos_sq = radians.cos().powi(2);
    let v0_squared = r2 * gravity / (r * sin_2 - 2.0 * y * cos_sq);
    if v0_squared < 0.0 || v0_squared.is_nan() {
        return None;
    }
    let v0 = v0_squared.sqrt();
    if v0 > max_velocity {
        return None;
    }
    let horizontal = v0 * radians.cos();
    let vertical = v0 * radians.sin();
    let (cos_xz, sin_xz) = (heading.cos(), heading.sin());
    let samples = (r / horizontal).ceil() as i32 * 2;
    let mut previous: Option<[f64; 3]> = None;
    let mut ri = 0.0;
    for _ in 0..samples - 1 {
        ri += r / f64::from(samples);
        let yi =
            (radians.sin() / radians.cos()) * ri - ri * ri * gravity / (2.0 * v0_squared * cos_sq);
        let sample = [from[0] + ri * cos_xz, from[1] + yi, from[2] + ri * sin_xz];
        if let Some(prev) = previous
            && !clear_transition(prev, sample, dimensions, cx)
        {
            return None;
        }
        previous = Some(sample);
    }
    Some([
        horizontal * cos_xz * JUMP_SCALE,
        vertical * JUMP_SCALE,
        horizontal * sin_xz * JUMP_SCALE,
    ])
}

fn box_at(position: [f64; 3], [width, height]: [f32; 2]) -> physics::Aabb {
    let half = f64::from(width) / 2.0;
    [
        position[0] - half,
        position[1],
        position[2] - half,
        position[0] + half,
        position[1] + f64::from(height),
        position[2] + half,
    ]
}

fn clear_transition(a: [f64; 3], b: [f64; 3], dimensions: [f32; 2], cx: &TickContext) -> bool {
    let direction = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let length = direction.iter().map(|c| c * c).sum::<f64>().sqrt();
    let smallest = f64::from(dimensions[0].min(dimensions[1]));
    let checks = (length / smallest).ceil() as i32;
    let mut point = a;
    for i in 0..checks {
        if i == checks - 1 {
            point = b;
        } else {
            let step = smallest * 0.899_999_976_158_142_1 / length;
            point = [
                point[0] + direction[0] * step,
                point[1] + direction[1] * step,
                point[2] + direction[2] * step,
            ];
        }
        if !physics::no_collision(box_at(point, dimensions), cx.block) {
            return false;
        }
    }
    true
}
