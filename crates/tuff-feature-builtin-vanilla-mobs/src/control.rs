//! The low-level controls goals steer a mob with: where it walks, where it
//! looks and when it jumps, plus how its body turns to follow its head.

use tuff_vanilla_data::registry::Attribute;

use super::MobCore;
use super::physics::block_pos;
use tuff_core::world::entity::TickContext;

/// Degrees in -180..180.
pub fn wrap_degrees(degrees: f32) -> f32 {
    let mut wrapped = degrees % 360.0;
    if wrapped >= 180.0 {
        wrapped -= 360.0;
    }
    if wrapped < -180.0 {
        wrapped += 360.0;
    }
    wrapped
}

pub fn degrees_difference(from: f32, to: f32) -> f32 {
    wrap_degrees(to - from)
}

/// Turns `from` towards `to` by at most `max` degrees.
pub fn rotate_towards(from: f32, to: f32, max: f32) -> f32 {
    from + degrees_difference(from, to).clamp(-max, max)
}

/// `target`, pulled back to within `max` degrees of `base`.
pub fn rotate_if_necessary(base: f32, target: f32, max: f32) -> f32 {
    let delta = degrees_difference(base, target).clamp(-max, max);
    target - delta
}

/// The yaw facing from one point towards another.
pub fn yaw_towards(dx: f64, dz: f64) -> f32 {
    (dz.atan2(dx) * 57.295_776_367_187_5) as f32 - 90.0
}

/// The right and forward axes a mob clinging to the block face `attached`
/// turns its head in: the client's frame for a body standing out of it.
pub fn attached_frame(attached: tuff_vanilla_data::block::Facing) -> ([f64; 3], [f64; 3]) {
    use tuff_vanilla_data::block::Facing;
    match attached {
        Facing::Down => ([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        Facing::Up => ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
        Facing::North => ([1.0, 0.0, 0.0], [0.0, -1.0, 0.0]),
        Facing::South => ([-1.0, 0.0, 0.0], [0.0, -1.0, 0.0]),
        Facing::East => ([0.0, 0.0, 1.0], [0.0, -1.0, 0.0]),
        Facing::West => ([0.0, 0.0, -1.0], [0.0, -1.0, 0.0]),
    }
}

pub fn pitch_towards(dy: f64, horizontal: f64) -> f32 {
    (-(dy.atan2(horizontal) * 57.295_776_367_187_5)) as f32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Operation {
    #[default]
    Wait,
    MoveTo,
    Strafe,
    Jumping,
}

/// How a mob's move control turns a wanted position into motion.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum MoveStyle {
    /// Walks there, jumping up steps.
    #[default]
    Walk,
    /// Floats there, nudged every few ticks (ghasts).
    Float { duration: i32 },
    /// Flies along its path in three dimensions, pitching towards each
    /// node; `hovers` keeps gravity off while idle (bees, parrots).
    Flying { hovers: bool, max_turn: f32 },
    /// Hops the way it faces (slimes).
    Hop(super::slime::Hop),
    /// Swims along its path, easing into its speed and rising or sinking
    /// towards each node (fish).
    Fish,
    /// Swims pitched towards each node, turning at most so fast (dolphins,
    /// axolotls, nautiluses).
    SmoothSwim(SmoothSwim),
    /// Swims like a fish with a wobble, eyeing the way ahead (guardians).
    Guardian,
    /// Walks, or swims when after something in the water or looking for
    /// land (drowned); `searching_for_land` is set by its swim-up goal.
    Drowned { searching_for_land: bool },
    /// Walks and swims, slowed on land and far from home (turtles).
    Turtle,
    /// Drifts straight there, through anything, and brakes on arrival
    /// (vexes).
    Phase,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SmoothSwim {
    pub max_turn_x: f32,
    pub max_turn_y: f32,
    pub in_water_speed: f32,
    pub outside_water_speed: f32,
    /// Buoyed up a little each tick in water, against its sinking.
    pub buoyant: bool,
}

/// How a mob's look control turns its head.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum LookStyle {
    #[default]
    Default,
    /// Tilts its head a little past what it looks at, never levels its
    /// pitch each tick, and drags its body along once the head turns more
    /// than `max_from_center` degrees from it (dolphins).
    SmoothSwim { max_from_center: f32 },
}

#[derive(Debug, Clone, Default)]
pub struct MoveControl {
    pub wanted: [f64; 3],
    pub speed_modifier: f64,
    pub operation: Operation,
    /// Forward and sideways input while strafing.
    pub strafe: (f32, f32),
}

impl MoveControl {
    pub fn has_wanted(&self) -> bool {
        self.operation == Operation::MoveTo
    }

    pub fn set_wanted(&mut self, wanted: [f64; 3], speed_modifier: f64) {
        self.wanted = wanted;
        self.speed_modifier = speed_modifier;
        if self.operation != Operation::Jumping {
            self.operation = Operation::MoveTo;
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LookControl {
    pub wanted: [f64; 3],
    y_max_speed: f32,
    x_max_angle: f32,
    cooldown: i32,
}

impl LookControl {
    pub fn set_look_at(&mut self, wanted: [f64; 3], y_max_speed: f32, x_max_angle: f32) {
        self.wanted = wanted;
        self.y_max_speed = y_max_speed;
        self.x_max_angle = x_max_angle;
        self.cooldown = 2;
    }

    pub fn is_looking(&self) -> bool {
        self.cooldown > 0
    }
}

#[derive(Debug, Clone, Default)]
pub struct BodyRotation {
    head_stable_time: i32,
    last_stable_head_yaw: f32,
}

pub const MAX_HEAD_X_ROT: f32 = 40.0;
pub const MAX_HEAD_Y_ROT: f32 = 75.0;
pub const HEAD_ROT_SPEED: f32 = 10.0;

impl MobCore {
    /// Walks forwards (or back) and sideways at a quarter of its speed,
    /// for one tick.
    pub fn strafe(&mut self, forwards: f32, right: f32) {
        self.movement.operation = Operation::Strafe;
        self.movement.strafe = (forwards, right);
        self.movement.speed_modifier = 0.25;
    }

    /// Walks forwards (or back) and sideways at its full movement speed,
    /// for one tick: a driving rider's steering, already scaled by the
    /// species (see `Species::drive_input`), rather than a wandering
    /// goal's quarter-speed nudge.
    pub fn strafe_full(&mut self, forwards: f32, right: f32) {
        self.movement.operation = Operation::Strafe;
        self.movement.strafe = (forwards, right);
        self.movement.speed_modifier = 1.0;
    }

    /// Turns the body itself towards `point` by at most the given angles.
    pub fn turn_to(&mut self, point: [f64; 3], max_yaw: f32, max_pitch: f32) {
        let eye = self.eye_position();
        let (dx, dy, dz) = (point[0] - eye[0], point[1] - eye[1], point[2] - eye[2]);
        let horizontal = (dx * dx + dz * dz).sqrt();
        let yaw = yaw_towards(dx, dz);
        let pitch = pitch_towards(dy, horizontal);
        self.base.yaw = rotate_towards(self.base.yaw, yaw, max_yaw);
        self.base.pitch = rotate_towards(self.base.pitch, pitch, max_pitch);
    }

    pub fn look_at_point(&mut self, point: [f64; 3]) {
        self.look
            .set_look_at(point, HEAD_ROT_SPEED, self.max_head_x_rot);
    }

    pub fn tick_move_control(&mut self, cx: &mut TickContext) {
        match self.move_style {
            MoveStyle::Walk => {}
            MoveStyle::Float { .. } => return self.tick_float_control(cx),
            MoveStyle::Flying { hovers, max_turn } => {
                return self.tick_flying_control(hovers, max_turn);
            }
            MoveStyle::Hop(_) => return self.tick_hop_control(cx),
            MoveStyle::Fish => return self.tick_fish_control(),
            MoveStyle::SmoothSwim(swim) => return self.tick_smooth_swim_control(swim),
            MoveStyle::Guardian => return self.tick_guardian_control(),
            MoveStyle::Drowned { searching_for_land } => {
                if self.tick_drowned_swim(searching_for_land, cx) {
                    return;
                }
            }
            MoveStyle::Turtle => return self.tick_turtle_control(),
            MoveStyle::Phase => return self.tick_phase_control(cx),
        }
        let speed_attribute = self.attribute(Attribute::MovementSpeed);
        match self.movement.operation {
            Operation::Strafe => {
                let speed = (self.movement.speed_modifier as f32) * speed_attribute as f32;
                let (mut forwards, mut right) = self.movement.strafe;
                let length = (forwards * forwards + right * right).sqrt().max(1.0);
                let scale = speed / length;
                let (xa, za) = (forwards * scale, right * scale);
                let (sin, cos) = (
                    self.base.yaw.to_radians().sin(),
                    self.base.yaw.to_radians().cos(),
                );
                let dx = xa * cos - za * sin;
                let dz = za * cos + xa * sin;
                let [x, y, z] = self.base.position;
                let pos = block_pos([x + f64::from(dx), y, z + f64::from(dz)]);
                if super::path::static_path_type(pos, cx.block, self.min_y)
                    != tuff_vanilla_data::entity::PathType::Walkable
                {
                    (forwards, right) = (1.0, 0.0);
                }
                self.set_speed(speed);
                self.body.zza = forwards;
                self.body.xxa = right;
                self.movement.operation = Operation::Wait;
            }
            Operation::Jumping => {
                self.set_speed((self.movement.speed_modifier * speed_attribute) as f32);
                if self.base.on_ground || self.in_liquid() {
                    self.movement.operation = Operation::Wait;
                }
            }
            Operation::Wait => self.body.zza = 0.0,
            Operation::MoveTo => {
                self.movement.operation = Operation::Wait;
                let [x, y, z] = self.base.position;
                let [wx, wy, wz] = self.movement.wanted;
                let (dx, dy, dz) = (wx - x, wy - y, wz - z);
                if dx * dx + dy * dy + dz * dz < 2.500_000_277_905_201e-7 {
                    self.body.zza = 0.0;
                    return;
                }
                let yaw = yaw_towards(dx, dz);
                self.base.yaw = rotate_towards_wrapped(self.base.yaw, yaw, 90.0);
                self.set_speed((self.movement.speed_modifier * speed_attribute) as f32);
                let pos = block_pos(self.base.position);
                let state = (cx.block)(pos);
                let top = state.and_then(|s| {
                    s.collision_boxes()
                        .iter()
                        .map(|b| b[4] as f64)
                        .reduce(f64::max)
                });
                let blocked_in = state.is_some_and(|s| {
                    !tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::DOORS, s.block())
                        && !tuff_datapack::tags::is(
                            tuff_vanilla_data::tag::block::key::FENCES,
                            s.block(),
                        )
                }) && top.is_some_and(|top| y < top + pos.y as f64);
                let step = self.max_up_step() as f64;
                if (dy > step && dx * dx + dz * dz < (self.size.width as f64).max(1.0))
                    || blocked_in
                {
                    self.jump.jump = true;
                    self.movement.operation = Operation::Jumping;
                }
            }
        }
    }

    fn tick_float_control(&mut self, cx: &mut TickContext) {
        if self.movement.operation != Operation::MoveTo {
            return;
        }
        let MoveStyle::Float { duration } = &mut self.move_style else {
            return;
        };
        let left = *duration;
        *duration -= 1;
        if left > 0 {
            return;
        }
        *duration += rand::Rng::gen_range(cx.rng, 0..5) + 2;
        let [x, y, z] = self.base.position;
        let [wx, wy, wz] = self.movement.wanted;
        let travel = [wx - x, wy - y, wz - z];
        let length = travel.iter().map(|c| c * c).sum::<f64>().sqrt();
        if length < 1.0e-4 || !self.can_float(travel, cx) {
            self.movement.operation = Operation::Wait;
            return;
        }
        let speed = self.attribute(Attribute::FlyingSpeed) * 5.0 / 3.0;
        for (axis, delta) in travel.iter().enumerate() {
            self.base.velocity[axis] += delta / length * speed;
        }
    }

    fn tick_phase_control(&mut self, cx: &TickContext) {
        if self.movement.operation != Operation::MoveTo {
            return;
        }
        let [x, y, z] = self.base.position;
        let [wx, wy, wz] = self.movement.wanted;
        let delta = [wx - x, wy - y, wz - z];
        let length = delta.iter().map(|c| c * c).sum::<f64>().sqrt();
        let [x0, y0, z0, x1, y1, z1] = self.bounds();
        let size = ((x1 - x0) + (y1 - y0) + (z1 - z0)) / 3.0;
        if length < size {
            self.movement.operation = Operation::Wait;
            self.base.velocity = self.base.velocity.map(|v| v * 0.5);
            return;
        }
        let scale = self.movement.speed_modifier * 0.05 / length;
        for (axis, d) in delta.iter().enumerate() {
            self.base.velocity[axis] += d * scale;
        }
        let [dx, dz] = match self.target_view(cx) {
            Some(target) => [target.position[0] - x, target.position[2] - z],
            None => [self.base.velocity[0], self.base.velocity[2]],
        };
        self.base.yaw = -(dx.atan2(dz) as f32) * 57.295_776;
        self.body_yaw = self.base.yaw;
    }

    fn tick_flying_control(&mut self, hovers: bool, max_turn: f32) {
        if self.movement.operation != Operation::MoveTo {
            if !hovers {
                self.base.no_gravity = false;
            }
            self.body.yya = 0.0;
            self.body.zza = 0.0;
            return;
        }
        self.movement.operation = Operation::Wait;
        self.base.no_gravity = true;
        let [dx, dy, dz] = self.wanted_delta();
        if dx * dx + dy * dy + dz * dz < 2.500_000_277_905_201e-7 {
            self.body.yya = 0.0;
            self.body.zza = 0.0;
            return;
        }
        self.base.yaw = rotate_towards_wrapped(self.base.yaw, yaw_towards(dx, dz), 90.0);
        let attribute = if self.base.on_ground {
            Attribute::MovementSpeed
        } else {
            Attribute::FlyingSpeed
        };
        let speed = (self.movement.speed_modifier * self.attribute(attribute)) as f32;
        self.set_speed(speed);
        let horizontal = (dx * dx + dz * dz).sqrt();
        if dy.abs() > 1.0e-5 || horizontal > 1.0e-5 {
            self.base.pitch =
                rotate_towards(self.base.pitch, pitch_towards(dy, horizontal), max_turn);
            self.body.yya = if dy > 0.0 { speed } else { -speed };
        }
    }

    fn wanted_delta(&self) -> [f64; 3] {
        let [x, y, z] = self.base.position;
        let [wx, wy, wz] = self.movement.wanted;
        [wx - x, wy - y, wz - z]
    }

    fn swim_target_speed(&self) -> f32 {
        (self.movement.speed_modifier * self.attribute(Attribute::MovementSpeed)) as f32
    }

    fn tick_fish_control(&mut self) {
        if self.body.eyes_in_water {
            self.base.velocity[1] += 0.005;
        }
        if self.movement.operation != Operation::MoveTo || self.navigation.is_done() {
            self.set_speed(0.0);
            return;
        }
        let target = self.swim_target_speed();
        self.set_speed(lerp(0.125, self.body.speed, target));
        let [dx, dy, dz] = self.wanted_delta();
        if dy != 0.0 {
            let length = (dx * dx + dy * dy + dz * dz).sqrt();
            self.base.velocity[1] += f64::from(self.body.speed) * (dy / length) * 0.1;
        }
        if dx != 0.0 || dz != 0.0 {
            self.base.yaw = rotate_towards_wrapped(self.base.yaw, yaw_towards(dx, dz), 90.0);
            self.body_yaw = self.base.yaw;
        }
    }

    fn tick_smooth_swim_control(&mut self, swim: SmoothSwim) {
        if swim.buoyant && self.body.in_water {
            self.base.velocity[1] += 0.005;
        }
        if self.movement.operation != Operation::MoveTo || self.navigation.is_done() {
            self.set_speed(0.0);
            self.body.xxa = 0.0;
            self.body.yya = 0.0;
            self.body.zza = 0.0;
            return;
        }
        let [dx, dy, dz] = self.wanted_delta();
        if dx * dx + dy * dy + dz * dz < 2.500_000_277_905_201e-7 {
            self.body.zza = 0.0;
            return;
        }
        let yaw = yaw_towards(dx, dz);
        self.base.yaw = rotate_towards_wrapped(self.base.yaw, yaw, swim.max_turn_y);
        self.body_yaw = self.base.yaw;
        self.head_yaw = self.base.yaw;
        let speed = self.swim_target_speed();
        if self.body.in_water {
            self.set_speed(speed * swim.in_water_speed);
            let horizontal = (dx * dx + dz * dz).sqrt();
            if dy.abs() > 1.0e-5 || horizontal.abs() > 1.0e-5 {
                let pitch = pitch_towards(dy, horizontal);
                let limited = wrap_degrees(pitch).clamp(-swim.max_turn_x, swim.max_turn_x);
                self.base.pitch = rotate_towards(self.base.pitch, limited, 5.0);
            }
            let radians = self.base.pitch * DEGREES_TO_RADIANS;
            self.body.zza = radians.cos() * speed;
            self.body.yya = -radians.sin() * speed;
            return;
        }
        let left_to_turn = wrap_degrees(self.base.yaw - yaw).abs();
        let turning = 1.0 - ((left_to_turn - 10.0) / 50.0).clamp(0.0, 1.0);
        self.set_speed(speed * swim.outside_water_speed * turning);
    }

    fn tick_guardian_control(&mut self) {
        if self.movement.operation != Operation::MoveTo || self.navigation.is_done() {
            self.set_speed(0.0);
            self.moving = false;
            return;
        }
        let delta = self.wanted_delta();
        let length = delta.iter().map(|c| c * c).sum::<f64>().sqrt();
        let [xd, yd, zd] = delta.map(|c| c / length);
        self.base.yaw =
            rotate_towards_wrapped(self.base.yaw, yaw_towards(delta[0], delta[2]), 90.0);
        self.body_yaw = self.base.yaw;
        let speed = lerp(0.125, self.body.speed, self.swim_target_speed());
        self.set_speed(speed);
        let phase = (self.tick_count as f64) + f64::from(self.base.id);
        let push = (phase * 0.5).sin() * 0.05;
        let radians = f64::from(self.base.yaw * DEGREES_TO_RADIANS);
        let (sin, cos) = (radians.sin(), radians.cos());
        let y_push = (phase * 0.75).sin() * 0.05;
        let velocity = &mut self.base.velocity;
        velocity[0] += push * cos;
        velocity[1] += y_push * (sin + cos) * 0.25 + f64::from(speed) * yd * 0.1;
        velocity[2] += push * sin;
        let [x, _, z] = self.base.position;
        let eye = self.eye_position()[1];
        let new = [x + xd * 2.0, eye + yd / length, z + zd * 2.0];
        let old = if self.look.is_looking() {
            self.look.wanted
        } else {
            new
        };
        let eased = [0, 1, 2].map(|axis| old[axis] + 0.125 * (new[axis] - old[axis]));
        self.look.set_look_at(eased, 10.0, 40.0);
        self.moving = true;
    }

    /// A drowned's swim, when it wants to swim and is in water; false hands
    /// over to walking (it sinks faster while not on the ground).
    fn tick_drowned_swim(&mut self, searching_for_land: bool, cx: &TickContext) -> bool {
        let target = self.target_view(cx);
        let wants = searching_for_land || target.is_some_and(|t| t.in_water);
        let swimming = wants && self.body.eyes_in_water;
        if swimming != self.swimming {
            self.swimming = swimming;
            self.changed(tuff_core::protocol::play::metadata::POSE);
        }
        if !(wants && self.body.in_water) {
            if !self.base.on_ground {
                self.base.velocity[1] -= 0.008;
            }
            return false;
        }
        if target.is_some_and(|t| t.position[1] > self.base.position[1]) || searching_for_land {
            self.base.velocity[1] += 0.002;
        }
        if self.movement.operation != Operation::MoveTo || self.navigation.is_done() {
            self.set_speed(0.0);
            return true;
        }
        let [dx, dy, dz] = self.wanted_delta();
        let length = (dx * dx + dy * dy + dz * dz).sqrt();
        self.base.yaw = rotate_towards_wrapped(self.base.yaw, yaw_towards(dx, dz), 90.0);
        self.body_yaw = self.base.yaw;
        let speed = lerp(0.125, self.body.speed, self.swim_target_speed());
        self.set_speed(speed);
        let speed = f64::from(speed);
        let velocity = &mut self.base.velocity;
        velocity[0] += speed * dx * 0.005;
        velocity[1] += speed * (dy / length) * 0.1;
        velocity[2] += speed * dz * 0.005;
        true
    }

    /// A turtle steers straight at each node, rising or sinking towards
    /// it; it is slower on land, away from home and as a baby.
    fn tick_turtle_control(&mut self) {
        if self.body.in_water {
            self.base.velocity[1] += 0.005;
            if !self.near_home(16.0) {
                self.set_speed((self.body.speed / 2.0).max(0.08));
            }
            if self.is_baby() {
                self.set_speed((self.body.speed / 3.0).max(0.06));
            }
        } else if self.base.on_ground {
            self.set_speed((self.body.speed / 2.0).max(0.06));
        }
        if self.movement.operation != Operation::MoveTo || self.navigation.is_done() {
            self.set_speed(0.0);
            return;
        }
        let [dx, dy, dz] = self.wanted_delta();
        let length = (dx * dx + dy * dy + dz * dz).sqrt();
        if length < 9.999_999_747_378_752e-6 {
            self.set_speed(0.0);
            return;
        }
        self.base.yaw = rotate_towards_wrapped(self.base.yaw, yaw_towards(dx, dz), 90.0);
        self.body_yaw = self.base.yaw;
        let speed = lerp(0.125, self.body.speed, self.swim_target_speed());
        self.set_speed(speed);
        self.base.velocity[1] += f64::from(speed) * (dy / length) * 0.1;
    }

    /// Whether its home (`bound_origin`) is within `distance` blocks of it.
    pub fn near_home(&self, distance: f64) -> bool {
        self.bound_origin
            .is_some_and(|home| self.distance_sq(home.center()) < distance * distance)
    }

    /// Whether its box can be swept along `travel` without meeting a
    /// block it does not already touch.
    fn can_float(&self, travel: [f64; 3], cx: &TickContext) -> bool {
        super::sweep::can_fly(self.bounds(), travel, cx.block)
    }

    /// Turns the head of a mob clinging to `face` in that face's frame:
    /// yaw only, measured around the face's outward normal.
    fn tick_attached_look(&mut self, face: tuff_vanilla_data::block::Facing) {
        self.base.pitch = 0.0;
        if self.look.cooldown <= 0 {
            self.head_yaw = rotate_towards(self.head_yaw, self.body_yaw, 10.0);
            return;
        }
        self.look.cooldown -= 1;
        let [x, _, z] = self.base.position;
        let eye = self.eye_position()[1];
        let [wx, wy, wz] = self.look.wanted;
        let delta = [wx - x, wy - eye, wz - z];
        let (right, forward) = attached_frame(face);
        let dot = |axis: [f64; 3]| axis[0] * delta[0] + axis[1] * delta[1] + axis[2] * delta[2];
        let (along_right, along_forward) = (dot(right), dot(forward));
        if along_right.abs() > 1.0e-5 || along_forward.abs() > 1.0e-5 {
            self.head_yaw = rotate_towards(
                self.head_yaw,
                yaw_towards(along_right, along_forward),
                self.look.y_max_speed,
            );
        }
    }

    pub fn turn_head(&mut self) {
        if let LookStyle::SmoothSwim { max_from_center } = self.look_style {
            return self.tick_smooth_swim_look(max_from_center);
        }
        if let Some(face) = self.attached {
            return self.tick_attached_look(face);
        }
        self.base.pitch = 0.0;
        if self.look.cooldown > 0 {
            self.look.cooldown -= 1;
            self.aim_head_at_wanted();
        } else {
            self.head_yaw = rotate_towards(self.head_yaw, self.body_yaw, 10.0);
        }
        if !self.navigation.is_done() {
            self.head_yaw = rotate_if_necessary(self.head_yaw, self.body_yaw, self.max_head_y_rot);
        }
    }

    /// Steps head yaw and pitch towards the look target, each only when
    /// the target is off that axis.
    fn aim_head_at_wanted(&mut self) {
        let [px, py, pz] = self.base.position;
        let [tx, ty, tz] = self.look.wanted;
        let (east, up, south) = (tx - px, ty - (py + self.size.eye_height as f64), tz - pz);
        let off = |v: f64| v.abs() > 1.0e-5;
        if off(south) || off(east) {
            self.head_yaw = rotate_towards(
                self.head_yaw,
                yaw_towards(east, south),
                self.look.y_max_speed,
            );
        }
        let flat = (east * east + south * south).sqrt();
        if off(up) || off(flat) {
            self.base.pitch = rotate_towards(
                self.base.pitch,
                pitch_towards(up, flat),
                self.look.x_max_angle,
            );
        }
    }

    fn tick_smooth_swim_look(&mut self, max_from_center: f32) {
        if self.look.cooldown > 0 {
            self.look.cooldown -= 1;
            let [x, y, z] = self.base.position;
            let eye = y + self.size.eye_height as f64;
            let [wx, wy, wz] = self.look.wanted;
            let (dx, dy, dz) = (wx - x, wy - eye, wz - z);
            if dz.abs() > 1.0e-5 || dx.abs() > 1.0e-5 {
                self.head_yaw = rotate_towards(
                    self.head_yaw,
                    yaw_towards(dx, dz) + 20.0,
                    self.look.y_max_speed,
                );
            }
            let horizontal = (dx * dx + dz * dz).sqrt();
            if dy.abs() > 1.0e-5 || horizontal.abs() > 1.0e-5 {
                self.base.pitch = rotate_towards(
                    self.base.pitch,
                    pitch_towards(dy, horizontal) + 10.0,
                    self.look.x_max_angle,
                );
            }
        } else {
            if self.navigation.is_done() {
                self.base.pitch = rotate_towards(self.base.pitch, 0.0, 5.0);
            }
            self.head_yaw = rotate_towards(self.head_yaw, self.body_yaw, self.look.y_max_speed);
        }
        let from_body = wrap_degrees(self.head_yaw - self.body_yaw);
        if from_body < -max_from_center {
            self.body_yaw -= 4.0;
        } else if from_body > max_from_center {
            self.body_yaw += 4.0;
        }
    }

    pub fn tick_jump_control(&mut self) {
        self.body.jumping = std::mem::take(&mut self.jump.jump);
    }

    /// Turns the body after the head and the walking direction, as clients
    /// would see it; the head yaw sent to clients is kept within reach of
    /// it.
    pub fn tick_body_rotation(&mut self) {
        if self.attached.is_some() {
            return;
        }
        let [x, _, z] = self.base.position;
        let [ox, _, oz] = self.body.previous;
        let moving = (x - ox).powi(2) + (z - oz).powi(2) > 2.500_000_277_905_201e-7;
        let rotation = &mut self.body_rotation;
        let max_head_y_rot = self.max_head_y_rot;
        if moving {
            self.body_yaw = self.base.yaw;
            self.head_yaw = rotate_if_necessary(self.head_yaw, self.body_yaw, max_head_y_rot);
            rotation.last_stable_head_yaw = self.head_yaw;
            rotation.head_stable_time = 0;
            return;
        }
        if (self.head_yaw - rotation.last_stable_head_yaw).abs() > 15.0 {
            rotation.head_stable_time = 0;
            rotation.last_stable_head_yaw = self.head_yaw;
            self.body_yaw = rotate_if_necessary(self.body_yaw, self.head_yaw, max_head_y_rot);
        } else {
            rotation.head_stable_time += 1;
            if rotation.head_stable_time > 10 {
                let since = rotation.head_stable_time - 10;
                let fraction = (since as f32 / 10.0).clamp(0.0, 1.0);
                let remaining = max_head_y_rot * (1.0 - fraction);
                self.body_yaw = rotate_if_necessary(self.body_yaw, self.head_yaw, remaining);
            }
        }
    }
}

const DEGREES_TO_RADIANS: f32 = 0.017_453_292;

fn lerp(delta: f32, from: f32, to: f32) -> f32 {
    from + delta * (to - from)
}

/// The move control's turn: towards `to` by at most `max`, kept in 0..360.
pub(super) fn rotate_towards_wrapped(from: f32, to: f32, max: f32) -> f32 {
    let mut result = from + wrap_degrees(to - from).clamp(-max, max);
    if result < 0.0 {
        result += 360.0;
    } else if result > 360.0 {
        result -= 360.0;
    }
    result
}

#[derive(Debug, Clone, Default)]
pub struct JumpControl {
    pub jump: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn the_head_turns_towards_the_look_target_then_back_to_the_body() {
        crate::tests::with_context(|mob, _cx| {
            mob.base.position = [0.5, 64.0, 0.5];
            mob.body_yaw = 40.0;
            mob.head_yaw = -20.0;
            mob.look.set_look_at([-6.0, 70.0, 9.0], 10.0, 30.0);
            let mut trace = Vec::new();
            for tick in 0..6 {
                if tick == 3 {
                    mob.look.set_look_at([4.0, 60.0, -2.0], 45.0, 15.0);
                }
                mob.turn_head();
                trace.push((mob.head_yaw.to_bits(), mob.base.pitch.to_bits()));
            }
            assert_eq!(
                trace,
                [
                    (3240099840, 3250434958),
                    (0, 3250434958),
                    (1092616192, 0),
                    (3255566336, 1097859072),
                    (3265265664, 1097859072),
                    (3263954944, 0),
                ]
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn angles_wrap_and_turn_like_the_game() {
        assert_eq!(wrap_degrees(190.0), -170.0);
        assert_eq!(wrap_degrees(-190.0), 170.0);
        assert_eq!(rotate_towards(0.0, 90.0, 10.0), 10.0);
        assert_eq!(rotate_towards(170.0, -170.0, 30.0), 190.0);
        assert_eq!(rotate_if_necessary(0.0, 100.0, 75.0), 25.0);
        assert!(yaw_towards(0.0, 1.0).abs() < 1e-4);
        assert!((yaw_towards(-1.0, 0.0) - 90.0).abs() < 1e-4);
        assert_eq!(rotate_towards_wrapped(350.0, 10.0, 90.0), 10.0);
    }
}
