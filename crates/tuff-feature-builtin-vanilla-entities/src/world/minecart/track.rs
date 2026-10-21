//! Minecart movement in a world with the minecart improvements feature
//! flag. A cart walks along its track in steps, one rail at a time, so it
//! can cover several blocks a tick up to the `max_minecart_speed` game
//! rule; slopes add speed in proportion to it, it sits 0.1 above the rail
//! and tilts on slopes, and viewers replay the steps it took rather than
//! interpolating between updates. It bumps into blocks and pushable
//! entities, pushes what it bumps into, and an empty passenger cart picks
//! up any mob it touches, however slow.

use super::*;

/// The feature flag that turns this movement on.
pub const FEATURE: &str = "minecraft:minecart_improvements";
/// The game rule capping a cart's speed, in blocks per second.
pub const MAX_SPEED_RULE: &str = "max_minecart_speed";
/// How far above its rail block a cart sits.
const RAIL_OFFSET: f64 = 0.1;
const EPSILON: f64 = 9.999999747378752e-6;
const SLOWDOWN_EMPTY: f64 = 0.975;
/// A cart this slow (squared) at the bottom of a V between two slopes
/// stops there.
const V_REST_SPEED_SQ: f64 = 0.005;
/// A slope adds this share of the cart's speed, or at least [`SLIDE`].
const SLOPE_GAIN: f64 = 0.02;
/// The game's single-precision square root of two: how much farther a
/// step goes on a slope.
const SLOPE_STRETCH: f64 = 1.4142135381698608;
const RIDER_PUSH_LIMIT_SQ: f64 = 0.01;
const BOOSTED_SPEED: f64 = 0.01;
const KICK_OFF: f64 = 0.2;
const PUSH_SCALE: f64 = 0.05000000074505806;
const CART_PUSH_SCALE: f64 = 0.10000000149011612 * 0.5;
/// Steps kept for viewers at most; more only build up while nobody takes
/// them (a cart riding another entity).
const MAX_PENDING_STEPS: usize = 256;
/// Steps a tick may take at most, against a malformed track.
const MAX_STEPS_PER_TICK: usize = 4096;

/// Whether carts in this world move the newer way.
pub fn enabled(cx: &TickContext) -> bool {
    cx.shared.datapacks.has_feature(FEATURE)
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: [f64; 3], by: f64) -> [f64; 3] {
    a.map(|c| c * by)
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn length_sq(a: [f64; 3]) -> f64 {
    dot(a, a)
}

fn length(a: [f64; 3]) -> f64 {
    length_sq(a).sqrt()
}

fn flat(a: [f64; 3]) -> [f64; 3] {
    [a[0], 0.0, a[2]]
}

fn flat_length(a: [f64; 3]) -> f64 {
    horizontal_sq(a).sqrt()
}

fn normalize(a: [f64; 3]) -> [f64; 3] {
    let length = length(a);
    if length < EPSILON {
        [0.0; 3]
    } else {
        scale(a, 1.0 / length)
    }
}

fn degrees(y: f64, x: f64) -> f32 {
    (y.atan2(x) * 180.0 / std::f64::consts::PI) as f32
}

fn rot_lerp(amount: f32, from: f32, to: f32) -> f32 {
    from + amount * wrap_degrees(to - from)
}

/// The rail exits of `shape` as offsets from the block's bottom centre.
fn half_exits(shape: Shape) -> Option<([f64; 3], [f64; 3])> {
    let (e0, e1) = exits(shape)?;
    let half = |e: [i32; 3]| e.map(|c| f64::from(c) * 0.5);
    Some((half(e0), half(e1)))
}

fn bottom_center(pos: BlockPos) -> [f64; 3] {
    [
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    ]
}

/// The horizontal unit direction a sloped rail climbs toward, or `None`
/// for a flat one.
fn uphill(shape: Shape) -> Option<[i32; 3]> {
    Some(match shape {
        Shape::AscendingEast => [1, 0, 0],
        Shape::AscendingWest => [-1, 0, 0],
        Shape::AscendingNorth => [0, 0, -1],
        Shape::AscendingSouth => [0, 0, 1],
        _ => return None,
    })
}

/// Whether heading along `heading` takes a cart down a rail of `shape`.
fn descending(heading: [f64; 3], shape: Shape) -> bool {
    uphill(shape).is_some_and(|up| {
        let climb = if up[0] != 0 {
            heading[0] * f64::from(up[0])
        } else {
            heading[2] * f64::from(up[2])
        };
        climb < 0.0
    })
}

/// The rail block a cart at `position` follows: the one it is in, or the
/// one just under its rail offset.
fn rail_cell(position: [f64; 3], block: BlockLookup) -> BlockPos {
    let [x, y, z] = position;
    let pos = block_of(x, y, z);
    let below = (y - RAIL_OFFSET - EPSILON).floor() as i32;
    let under = BlockPos::new(pos.x, below, pos.z);
    if block(under).is_some_and(is_rail_block) {
        under
    } else {
        pos
    }
}

/// Blocks a cart on the rail at `rail` moves through: the one under the
/// rail and, on a slope, the one the slope climbs into.
fn passable(rail: BlockPos, state: Option<BlockState>) -> [Option<BlockPos>; 2] {
    let Some(shape) = rail_shape(state) else {
        return [None; 2];
    };
    let below = BlockPos::new(rail.x, rail.y - 1, rail.z);
    let climbed = uphill(shape).map(|up| BlockPos::new(rail.x + up[0], rail.y, rail.z + up[2]));
    [Some(below), climbed]
}

/// The travel a cart has left this tick and which one-off speed changes it
/// has already taken.
#[derive(Debug)]
struct TickBudget {
    travel_left: f64,
    opening: bool,
    slope_used: bool,
    halted: bool,
    boost_used: bool,
}

impl TickBudget {
    fn new() -> Self {
        Self {
            travel_left: 0.0,
            opening: true,
            slope_used: false,
            halted: false,
            boost_used: false,
        }
    }

    fn keep_going(&self) -> bool {
        self.opening || self.travel_left > EPSILON
    }
}

/// Swaps `v` for `candidate` when that changes its horizontal speed.
fn adopt_if_changed(candidate: [f64; 3], v: &mut [f64; 3]) -> bool {
    if horizontal_sq(candidate) == horizontal_sq(*v) {
        return false;
    }
    *v = candidate;
    true
}

/// The yaw a cart heading along `heading` faces, turned round when
/// `flipped`.
fn heading_yaw(heading: [f64; 3], flipped: bool) -> f32 {
    180.0 - degrees(heading[2], heading[0]) + if flipped { 180.0 } else { 0.0 }
}

/// Where a step along a rail of `shape` at `pos` heads for, and whether
/// that rail is a slope.
struct RailExit {
    point: [f64; 3],
    hill: bool,
}

fn rail_exit(pos: BlockPos, shape: Shape, heading: [f64; 3]) -> Option<RailExit> {
    let (a, b) = half_exits(shape)?;
    let (a_flat, b_flat) = (flat(a), flat(b));
    let side = if dot(heading, a_flat) < dot(heading, b_flat) {
        b_flat
    } else {
        a_flat
    };
    let hill = a[1] != b[1];
    let lifted = add(add(bottom_center(pos), side), [0.0, RAIL_OFFSET, 0.0]);
    let mut point = add(lifted, scale(normalize(side), EPSILON));
    if hill && !descending(heading, shape) {
        point[1] += 1.0;
    }
    Some(RailExit { point, hill })
}

/// Something a cart can bump into while it moves this tick.
#[derive(Debug, Clone, Copy)]
struct Obstacle {
    bounds: physics::Aabb,
    id: i32,
    position: [f64; 3],
    velocity: [f64; 3],
    kind: Option<EntityType>,
    /// Something rides it.
    carries: bool,
    /// It rides something.
    riding: bool,
}

impl Obstacle {
    fn is_cart(&self) -> bool {
        self.kind.is_some_and(is_minecart)
    }

    fn is_player(&self) -> bool {
        self.kind.is_none()
    }
}

/// The entities a cart may bump into this tick, with the piston-moved
/// blocks, as collision boxes.
#[derive(Debug, Default)]
struct Obstacles {
    entities: Vec<Obstacle>,
    boxes: Vec<physics::Aabb>,
    /// The cart took in a mob this tick, which rides it from now on.
    picked_up: bool,
}

impl Obstacles {
    fn rebuild(&mut self, pistons: &[physics::Aabb]) {
        self.boxes.clear();
        self.boxes.extend_from_slice(pistons);
        self.boxes.extend(self.entities.iter().map(|o| o.bounds));
    }

    fn remove(&mut self, id: i32, pistons: &[physics::Aabb]) {
        self.entities.retain(|o| o.id != id);
        self.rebuild(pistons);
    }
}

impl Minecart {
    /// A new cart placed in a world with the newer movement starts on the
    /// rail it was put on.
    pub fn place_on_rails(&mut self, block: BlockLookup) {
        let pos = rail_cell(self.base.position, block);
        self.adjust_to_rails(pos, block(pos), true);
    }

    /// The speed cap the game rule sets for any cart, in blocks per tick.
    fn rule_speed(&self, cx: &TickContext) -> f64 {
        let rule = f64::from(cx.shared.game_rules.borrow().int(MAX_SPEED_RULE));
        let water = if self.in_water { 0.5 } else { 1.0 };
        rule * water / 20.0
    }

    /// This cart's speed cap: a furnace minecart's is lower.
    pub(super) fn track_max_speed(&self, cx: &TickContext) -> f64 {
        let speed = self.rule_speed(cx);
        match (self.is_furnace(), self.in_water) {
            (true, true) => speed * 0.75,
            (true, false) => speed * 0.5,
            _ => speed,
        }
    }

    fn carries(&self) -> bool {
        !self.base.riders.is_empty() || !self.base.entity_riders.is_empty()
    }

    fn record(&mut self, weight: f32) {
        if self.base.vehicle.is_some() {
            return;
        }
        if self.steps.len() >= MAX_PENDING_STEPS {
            self.steps.remove(0);
        }
        self.steps.push(play::TrackStep {
            position: self.base.position,
            velocity: self.base.velocity,
            yaw: self.base.yaw,
            pitch: self.base.pitch,
            weight,
        });
    }

    /// Turns to `yaw` and `pitch`, flipping round instead of turning half
    /// round.
    fn set_rotation(&mut self, mut yaw: f32, mut pitch: f32) {
        let turned = f64::from((yaw - self.base.yaw).abs());
        if (175.0..=185.0).contains(&turned) {
            self.flipped = !self.flipped;
            yaw -= 180.0;
            pitch *= -1.0;
        }
        self.base.pitch = (pitch.clamp(-45.0, 45.0) % 360.0).clamp(-90.0, 90.0);
        self.base.yaw = yaw % 360.0;
    }

    /// Lays it on the rail at `pos` (if that is a rail), facing along it:
    /// onto the rail's line, 0.1 above it and as far up a slope as it is
    /// along. `instant` shows it there to viewers at once.
    pub(super) fn adjust_to_rails(
        &mut self,
        pos: BlockPos,
        state: Option<BlockState>,
        instant: bool,
    ) {
        let Some(shape) = rail_shape(state) else {
            return;
        };
        let Some((a, b)) = half_exits(shape) else {
            return;
        };
        let (mut front, mut back) = (flat(a), flat(b));
        let v = self.base.velocity;
        let heads_back = length(v) > EPSILON && dot(v, front) < dot(v, back);
        if heads_back || descending(back, shape) {
            std::mem::swap(&mut front, &mut back);
        }
        let before = self.base.position;
        let base = bottom_center(pos);
        let (on_line, facing) = if a[0] != b[0] && a[2] != b[2] {
            let chord = sub(b, a);
            let offset = sub(sub(before, base), a);
            let projected = scale(chord, dot(chord, offset) / dot(chord, chord));
            (add(add(base, a), projected), projected)
        } else {
            let runs_along_x = a[0] != b[0];
            let runs_along_z = a[2] != b[2];
            let point = [
                if runs_along_z { base[0] } else { before[0] },
                f64::from(pos.y),
                if runs_along_x { base[2] } else { before[2] },
            ];
            (point, front)
        };
        let yaw = heading_yaw(facing, self.flipped);
        self.base.position = on_line;
        let mut pitch = 0.0;
        let mut lift = RAIL_OFFSET;
        if a[1] != b[1] {
            lift += length(sub(add(base, back), on_line));
            pitch = if self.flipped { 45.0 } else { -45.0 };
        }
        self.base.position[1] += lift;
        self.set_rotation(yaw, pitch);
        let shifted = length(sub(before, self.base.position));
        if shifted > 0.0 {
            self.record(if instant { 0.0 } else { shifted as f32 });
        }
    }

    /// A tick's movement the newer way: gravity, then one step per rail
    /// until the tick's travel is spent.
    pub(super) fn tick_on_track(&mut self, cx: &mut TickContext) -> Moved {
        if self.first_tick {
            let pos = rail_cell(self.base.position, cx.block);
            let state = (cx.block)(pos);
            self.on_rails = state.is_some_and(is_rail_block);
            self.adjust_to_rails(pos, state, true);
        }
        if !self.base.no_gravity {
            let gravity = if self.in_water {
                WATER_GRAVITY
            } else {
                GRAVITY
            };
            self.base.velocity[1] -= gravity;
        }
        self.step_start = self.base.position;
        let mut obstacles = self.obstacles(cx);
        let mut moved = Moved::default();
        let mut budget = TickBudget::new();
        let mut swept = Vec::new();
        for _ in 0..MAX_STEPS_PER_TICK {
            if !budget.keep_going() || self.broken {
                break;
            }
            let entry_velocity = self.base.velocity;
            let cell = rail_cell(self.base.position, cx.block);
            let found = (cx.block)(cell);
            let rail = found.filter(|state| is_rail_block(*state));
            if self.on_rails != rail.is_some() {
                self.on_rails = rail.is_some();
                self.adjust_to_rails(cell, found, false);
            }
            match rail {
                Some(state) => {
                    let Some(shape) = state.shape() else {
                        break;
                    };
                    self.ride_rail(
                        cx,
                        &mut obstacles,
                        &mut budget,
                        (cell, state, shape),
                        entry_velocity,
                        &mut moved,
                    );
                }
                None => {
                    self.leave_track(cx, &mut obstacles, &mut moved);
                    budget.travel_left = 0.0;
                }
            }
            self.face_and_record(cx, entry_velocity);
            swept.push(self.base.bounds());
            budget.opening = false;
        }
        swept.pop();
        if !swept.is_empty() {
            cx.effects.push(Effect::PassedThrough(swept));
        }
        moved
    }

    /// One step on the rail at `cell`: its speed changes, then travel
    /// along it.
    fn ride_rail(
        &mut self,
        cx: &mut TickContext,
        obstacles: &mut Obstacles,
        budget: &mut TickBudget,
        (cell, state, shape): (BlockPos, BlockState, Shape),
        entry_velocity: [f64; 3],
        moved: &mut Moved,
    ) {
        self.base.fall_distance = 0.0;
        self.step_start = self.base.position;
        if state.block() == Block::ActivatorRail {
            self.activate(state.powered() == Some(true), cx);
        }
        let entry_speed = flat_length(entry_velocity);
        let v = self.track_speed(cx, flat(entry_velocity), budget, cell, state, shape);
        let gained = flat_length(v);
        budget.travel_left = if budget.opening {
            gained
        } else {
            budget.travel_left + (gained - entry_speed)
        };
        self.base.velocity = v;
        budget.travel_left =
            self.step_along_track(cx, obstacles, cell, shape, budget.travel_left, moved);
    }

    /// Faces the step it just took and records it for viewers.
    fn face_and_record(&mut self, cx: &TickContext, entry_velocity: [f64; 3]) {
        let delta = sub(self.base.position, self.step_start);
        let stepped = length(delta);
        if stepped <= EPSILON {
            if horizontal_sq(entry_velocity) > 0.0 {
                self.record(1.0);
            }
            return;
        }
        if horizontal_sq(delta) > EPSILON {
            let tilt = if self.on_rails || !self.base.on_ground {
                90.0 - degrees(flat_length(delta), delta[1])
            } else {
                0.0
            };
            let tilt = if self.flipped { -tilt } else { tilt };
            self.set_rotation(heading_yaw(delta, self.flipped), tilt);
        } else if !self.on_rails {
            self.base.pitch = if self.base.on_ground {
                0.0
            } else {
                rot_lerp(0.2, self.base.pitch, 0.0)
            };
        }
        self.record(stepped.min(self.rule_speed(cx)) as f32);
    }

    /// Its speed along the rail at `pos` this step: slope, the rider's
    /// keys, a brake, friction and the speed cap, then a boost. Each of
    /// slope, halt and boost applies at most once a tick.
    fn track_speed(
        &mut self,
        cx: &TickContext,
        mut v: [f64; 3],
        budget: &mut TickBudget,
        pos: BlockPos,
        state: BlockState,
        shape: Shape,
    ) -> [f64; 3] {
        if !budget.slope_used {
            budget.slope_used = adopt_if_changed(self.slope_speed(v, shape), &mut v);
        }
        if budget.opening && adopt_if_changed(self.rider_speed(v), &mut v) {
            budget.halted = true;
        }
        if !budget.halted {
            budget.halted = adopt_if_changed(brake_speed(v, state), &mut v);
        }
        if budget.opening {
            v = self.slowed(v, self.friction());
            if length_sq(v) > 0.0 {
                let capped = length(v).min(self.track_max_speed(cx));
                v = scale(normalize(v), capped);
            }
        }
        if !budget.boost_used {
            budget.boost_used = adopt_if_changed(boost_speed(v, pos, state, cx.block), &mut v);
        }
        v
    }

    fn friction(&self) -> f64 {
        match self.container_slowdown() {
            Some(keep) => keep,
            None if self.carries() => SLOWDOWN_RIDDEN,
            None => SLOWDOWN_EMPTY,
        }
    }

    fn slope_speed(&self, v: [f64; 3], shape: Shape) -> [f64; 3] {
        let Some(up) = uphill(shape) else {
            return v;
        };
        let mut slide = SLIDE.max(flat_length(v) * SLOPE_GAIN);
        if self.in_water {
            slide *= 0.2;
        }
        let mut pull = [0.0; 3];
        for axis in [0, 2] {
            if up[axis] != 0 {
                pull[axis] = -slide * f64::from(up[axis]);
            }
        }
        add(v, pull)
    }

    /// A rider's movement keys start a cart that is nearly stopped.
    fn rider_speed(&self, v: [f64; 3]) -> [f64; 3] {
        if self.base.riders.is_empty() {
            return v;
        }
        let intent = move_intent(self.input);
        if length_sq(intent) <= 0.0 || horizontal_sq(v) >= RIDER_PUSH_LIMIT_SQ {
            return v;
        }
        let toward = normalize(intent);
        add(v, scale(normalize(flat(toward)), RIDER_NUDGE))
    }

    /// Travels up to `budget` along the rail at `pos` toward the end it is
    /// heading for, stopping there; returns the travel left for the next
    /// rail.
    fn step_along_track(
        &mut self,
        cx: &mut TickContext,
        obstacles: &mut Obstacles,
        pos: BlockPos,
        shape: Shape,
        budget: f64,
        moved: &mut Moved,
    ) -> f64 {
        if budget < EPSILON {
            return 0.0;
        }
        let heading = flat(self.base.velocity);
        let Some(exit) = rail_exit(pos, shape, heading) else {
            return 0.0;
        };
        let speed = length(heading);
        if speed < EPSILON {
            self.base.velocity = [0.0; 3];
            return 0.0;
        }
        let start = self.base.position;
        let aim = normalize(sub(exit.point, start));
        let velocity = scale(aim, speed / flat_length(aim));
        let stretch = if exit.hill { SLOPE_STRETCH } else { 1.0 };
        let planned = add(start, scale(normalize(velocity), budget * stretch));
        let (target, carry) = if length_sq(sub(exit.point, start)) <= length_sq(sub(planned, start))
        {
            (exit.point, flat_length(sub(exit.point, planned)))
        } else {
            (planned, 0.0)
        };
        self.travel(cx, obstacles, sub(target, start), moved);
        if exit.hill && self.settle_on_slope(cx, shape, &exit, target, velocity) {
            return 0.0;
        }
        let stalled = length(sub(self.base.position, start)) < EPSILON;
        if stalled && length(sub(target, start)) > EPSILON {
            self.base.velocity = [0.0; 3];
            return 0.0;
        }
        self.base.velocity = velocity;
        carry
    }

    /// After a step on a slope: stops in a dip, or keeps the cart from
    /// sinking under the slope's surface. True when it stopped.
    fn settle_on_slope(
        &mut self,
        cx: &TickContext,
        shape: Shape,
        exit: &RailExit,
        target: [f64; 3],
        velocity: [f64; 3],
    ) -> bool {
        let landing = (cx.block)(block_of(target[0], target[1], target[2]));
        if let Some(next) = rail_shape(landing)
            && self.halts_in_dip(shape, next)
        {
            return true;
        }
        let gap = flat_length(sub(exit.point, self.base.position));
        let surface = exit.point[1]
            + if descending(velocity, shape) {
                gap
            } else {
                -gap
            };
        self.base.position[1] = self.base.position[1].max(surface);
        false
    }

    /// A slow cart rolling down `current` onto the foot of slope `next`
    /// stops between them.
    fn halts_in_dip(&mut self, current: Shape, next: Shape) -> bool {
        let v = self.base.velocity;
        let dip = is_slope(next) && descending(v, current) && !descending(v, next);
        if dip && length_sq(v) < V_REST_SPEED_SQ {
            self.base.velocity = [0.0; 3];
            return true;
        }
        false
    }

    /// Off the rails: it rolls and falls freely, capped at its top speed.
    fn leave_track(&mut self, cx: &mut TickContext, obstacles: &mut Obstacles, moved: &mut Moved) {
        let max = self.track_max_speed(cx);
        let v = &mut self.base.velocity;
        v[0] = v[0].clamp(-max, max);
        v[2] = v[2].clamp(-max, max);
        if self.base.on_ground {
            *v = scale(*v, 0.5);
        }
        let delta = self.base.velocity;
        self.travel(cx, obstacles, delta, moved);
        if !self.base.on_ground {
            self.base.velocity = scale(self.base.velocity, AIR_DRAG);
        }
    }

    /// What it may bump into this tick: pushable entities near enough to
    /// reach, and the blocks pistons are moving.
    fn obstacles(&self, cx: &TickContext) -> Obstacles {
        let reach = physics::inflate(
            self.base.bounds(),
            [MOVING_BLOCK_REACH + self.track_max_speed(cx); 3],
        );
        let id = self.base.id;
        let mut obstacles = Obstacles::default();
        for other in cx.entities {
            let pushable = is_minecart(other.kind)
                || super::super::boat::is_boat(other.kind)
                || other.living.is_some_and(|l| l.alive);
            if other.id == id
                || !pushable
                || other.vehicle == Some(id)
                || self.base.entity_riders.contains(&other.id)
                || !physics::intersects(other.bounds, reach)
            {
                continue;
            }
            obstacles.entities.push(Obstacle {
                bounds: other.bounds,
                id: other.id,
                position: other.position,
                velocity: other.velocity,
                kind: Some(other.kind),
                carries: other.carries,
                riding: other.vehicle.is_some(),
            });
        }
        for player in cx.players {
            if player.vehicle == Some(id)
                || self.base.riders.iter().any(|r| r.id == player.id)
                || !physics::intersects(player.bounds(), reach)
            {
                continue;
            }
            obstacles.entities.push(Obstacle {
                bounds: player.bounds(),
                id: player.id,
                position: player.position,
                velocity: player.movement,
                kind: None,
                carries: false,
                riding: player.vehicle.is_some(),
            });
        }
        obstacles.rebuild(&self.moving_blocks);
        obstacles
    }

    /// Moves by `delta` as the cart moves itself: an empty passenger cart
    /// takes in a mob in its way, it stops at blocks (except the one under
    /// its rail and the one its slope climbs into) and at entities, and
    /// pushes what it bumped into.
    fn travel(
        &mut self,
        cx: &mut TickContext,
        obstacles: &mut Obstacles,
        delta: [f64; 3],
        moved: &mut Moved,
    ) {
        if self.rideable() && !self.carries() && !obstacles.picked_up {
            let bounds = self.base.bounds();
            let ahead = physics::offset(bounds, delta);
            let swept = [
                bounds[0].min(ahead[0]),
                bounds[1].min(ahead[1]),
                bounds[2].min(ahead[2]),
                bounds[3].max(ahead[3]),
                bounds[4].max(ahead[4]),
                bounds[5].max(ahead[5]),
            ];
            let area = physics::inflate(swept, [0.2, 0.0, 0.2]);
            let mounted = super::super::pick_up_riders(&self.base, 1, area, cx, |kind, _| {
                kind != EntityType::IronGolem
            });
            for id in mounted {
                obstacles.picked_up = true;
                obstacles.remove(id, &self.moving_blocks);
            }
        }
        let block = cx.block;
        let rail = rail_cell(self.base.position, block);
        let skip = passable(rail, block(rail));
        let shape = |pos: BlockPos| {
            if skip.contains(&Some(pos)) {
                physics::Shape {
                    boxes: &[],
                    offset: [0.0; 3],
                }
            } else {
                physics::Shape::collision(block(pos), pos)
            }
        };
        let step = collide_move(&mut self.base, delta, &shape, &obstacles.boxes);
        moved.horizontal_collision |= step.horizontal_collision;
        moved.vertical_collision |= step.vertical_collision;
        if step.landed.is_some() {
            moved.landed = step.landed;
        }
        if step.horizontal_collision || step.vertical_collision {
            self.push_bumped(cx, obstacles);
        }
    }

    /// Pushes what it bumped into: a passenger cart pushes players, iron
    /// golems, other carts and (when it carries someone) anything; any
    /// other cart pushes only carts.
    fn push_bumped(&mut self, cx: &mut TickContext, obstacles: &Obstacles) {
        let area = physics::inflate(self.base.bounds(), [1.0e-7; 3]);
        let rideable = self.rideable();
        let carries = self.carries() || obstacles.picked_up;
        for other in obstacles
            .entities
            .iter()
            .filter(|o| physics::intersects(o.bounds, area))
        {
            if other.is_cart() {
                self.push_cart_along(other, cx);
            } else if rideable
                && (other.is_player()
                    || other.kind == Some(EntityType::IronGolem)
                    || carries
                    || other.riding)
            {
                self.shove(other, carries, cx);
            }
        }
    }

    /// The other entity's push away from this cart, and this cart's away
    /// from it.
    fn shove(&mut self, other: &Obstacle, carries: bool, cx: &mut TickContext) {
        let xa = self.base.position[0] - other.position[0];
        let za = self.base.position[2] - other.position[2];
        let dd = xa.abs().max(za.abs());
        if dd < 0.009999999776482582 {
            return;
        }
        let dd = dd.sqrt();
        let pow = (1.0 / dd).min(1.0);
        let (xa, za) = (xa / dd * pow * PUSH_SCALE, za / dd * pow * PUSH_SCALE);
        if !other.carries {
            cx.effects.push(Effect::Nudge {
                target: other.id,
                push: [-xa, 0.0, -za],
            });
        }
        if !carries {
            self.base.velocity[0] += xa;
            self.base.velocity[2] += za;
        }
    }

    /// Another cart's push from this one, as the newer movement does it:
    /// whatever their headings, they trade speed, a furnace minecart
    /// keeping most of its own.
    fn push_cart_along(&mut self, other: &Obstacle, cx: &mut TickContext) {
        let xa = self.base.position[0] - other.position[0];
        let za = self.base.position[2] - other.position[2];
        let dd = xa * xa + za * za;
        if dd < 9.999999747378752e-5 {
            return;
        }
        let dd = dd.sqrt();
        let pow = (1.0 / dd).min(1.0) * CART_PUSH_SCALE;
        let (xa, za) = (xa / dd * pow, za / dd * pow);
        let mine = self.base.velocity;
        let theirs = other.velocity;
        let other_furnace = other.kind == Some(EntityType::FurnaceMinecart);
        let (my_new, their_new) = if self.is_furnace() && !other_furnace {
            (
                [mine[0] * 0.95, mine[1], mine[2] * 0.95],
                [
                    theirs[0] * 0.2 + mine[0] - xa,
                    theirs[1],
                    theirs[2] * 0.2 + mine[2] - za,
                ],
            )
        } else if !self.is_furnace() && other_furnace {
            (
                [
                    mine[0] * 0.2 + theirs[0] + xa,
                    mine[1],
                    mine[2] * 0.2 + theirs[2] + za,
                ],
                [theirs[0] * 0.95, theirs[1], theirs[2] * 0.95],
            )
        } else {
            let (xdd, zdd) = ((theirs[0] + mine[0]) / 2.0, (theirs[2] + mine[2]) / 2.0);
            (
                [mine[0] * 0.2 + xdd + xa, mine[1], mine[2] * 0.2 + zdd + za],
                [
                    theirs[0] * 0.2 + xdd - xa,
                    theirs[1],
                    theirs[2] * 0.2 + zdd - za,
                ],
            )
        };
        self.base.velocity = my_new;
        cx.effects.push(Effect::Nudge {
            target: other.id,
            push: sub(their_new, theirs),
        });
    }
}

/// An unpowered powered rail halves a cart's speed, stopping a slow one.
fn brake_speed(v: [f64; 3], state: BlockState) -> [f64; 3] {
    if state.block() != Block::PoweredRail || state.powered() != Some(false) {
        return v;
    }
    if length(v) < BRAKE_STOP_SPEED {
        [0.0; 3]
    } else {
        scale(v, 0.5)
    }
}

/// A powered rail adds to a moving cart's speed and starts a stopped one
/// away from a solid block at one end.
fn boost_speed(v: [f64; 3], pos: BlockPos, state: BlockState, block: BlockLookup) -> [f64; 3] {
    if state.block() != Block::PoweredRail || state.powered() != Some(true) {
        return v;
    }
    let speed = length(v);
    if speed > BOOSTED_SPEED {
        return scale(normalize(v), speed + BOOST);
    }
    let conductor = |dx: i32, dz: i32| {
        block(BlockPos::new(pos.x + dx, pos.y, pos.z + dz))
            .is_some_and(|state| state.is_redstone_conductor())
    };
    let away = match state.shape() {
        Some(Shape::EastWest) if conductor(-1, 0) => [1.0, 0.0, 0.0],
        Some(Shape::EastWest) if conductor(1, 0) => [-1.0, 0.0, 0.0],
        Some(Shape::NorthSouth) if conductor(0, -1) => [0.0, 0.0, 1.0],
        Some(Shape::NorthSouth) if conductor(0, 1) => [0.0, 0.0, -1.0],
        _ => return v,
    };
    scale(away, speed + KICK_OFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rail(shape: Shape) -> BlockState {
        Block::Rail.default_state().with_shape(shape).unwrap()
    }

    fn cart_at(position: [f64; 3], velocity: [f64; 3]) -> Minecart {
        Minecart::with_base(EntityBase::new(
            EntityType::Minecart,
            1,
            1,
            position,
            velocity,
        ))
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_cart_sits_a_tenth_above_a_flat_rail_and_on_its_line() {
        let mut cart = cart_at([4.3, 64.0625, 7.9], [0.0; 3]);
        let state = Some(rail(Shape::EastWest));
        cart.adjust_to_rails(BlockPos::new(4, 64, 7), state, true);
        assert_eq!(cart.base.position, [4.3, 64.1, 7.5]);
        assert_eq!(cart.steps.len(), 1);
        assert_eq!(cart.steps[0].weight, 0.0, "placed at once");
        assert_eq!(cart.base.pitch, 0.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_cart_on_a_slope_tilts_and_rides_up_it() {
        let mut cart = cart_at([4.5, 64.5, 7.5], [0.1, 0.0, 0.0]);
        let state = Some(rail(Shape::AscendingEast));
        cart.adjust_to_rails(BlockPos::new(4, 64, 7), state, false);
        assert!(
            (cart.base.position[1] - 64.6).abs() < 1.0e-9,
            "halfway up: {:?}",
            cart.base.position
        );
        assert_eq!(cart.base.pitch.abs(), 45.0);
        assert!(cart.steps[0].weight > 0.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_curve_puts_a_cart_on_its_diagonal() {
        let mut cart = cart_at([4.9, 64.0625, 7.9], [0.0; 3]);
        let state = Some(rail(Shape::SouthEast));
        cart.adjust_to_rails(BlockPos::new(4, 64, 7), state, true);
        let [x, y, z] = cart.base.position;
        assert!((y - 64.1).abs() < 1.0e-9);
        assert!(
            (x - 4.75).abs() < 1.0e-9 && (z - 7.75).abs() < 1.0e-9,
            "on the line between the south and east exits: {:?}",
            cart.base.position
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn turning_half_round_flips_the_cart_instead() {
        let mut cart = cart_at([0.0; 3], [0.0; 3]);
        cart.base.yaw = 90.0;
        cart.set_rotation(270.0, 10.0);
        assert!(cart.flipped);
        assert_eq!(cart.base.yaw, 90.0);
        assert_eq!(cart.base.pitch, -10.0);
        cart.set_rotation(100.0, 80.0);
        assert!(cart.flipped, "a small turn keeps it");
        assert_eq!(cart.base.pitch, 45.0, "pitch is capped");
    }

    #[tuff_core::watchdog]
    #[test]
    fn powered_rails_boost_brake_and_kick_off_a_wall() {
        let on = Block::PoweredRail
            .default_state()
            .with_shape(Shape::EastWest)
            .unwrap()
            .with("powered", "true")
            .unwrap();
        let off = on.with("powered", "false").unwrap();
        let pos = BlockPos::new(0, 64, 0);
        let air = |_: BlockPos| Some(Block::Air.default_state());
        let boosted = boost_speed([0.3, 0.0, 0.0], pos, on, &air);
        assert!((boosted[0] - 0.36).abs() < 1.0e-12);
        let wall = |p: BlockPos| {
            Some(if p.x == -1 {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        assert_eq!(boost_speed([0.0; 3], pos, on, &wall), [0.2, 0.0, 0.0]);
        assert_eq!(boost_speed([0.0; 3], pos, on, &air), [0.0; 3]);
        assert_eq!(brake_speed([0.02, 0.0, 0.0], off), [0.0; 3]);
        assert_eq!(brake_speed([0.4, 0.0, 0.0], off), [0.2, 0.0, 0.0]);
        assert_eq!(brake_speed([0.4, 0.0, 0.0], on), [0.4, 0.0, 0.0]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_rail_under_a_raised_cart_is_the_one_it_follows() {
        let level = |pos: BlockPos| {
            Some(if pos == BlockPos::new(0, 64, 0) {
                rail(Shape::EastWest)
            } else {
                Block::Air.default_state()
            })
        };
        assert_eq!(rail_cell([0.5, 64.1, 0.5], &level), BlockPos::new(0, 64, 0));
        assert_eq!(
            rail_cell([0.5, 65.05, 0.5], &level),
            BlockPos::new(0, 64, 0)
        );
        assert_eq!(rail_cell([0.5, 65.2, 0.5], &level), BlockPos::new(0, 65, 0));
        assert_eq!(
            passable(BlockPos::new(0, 64, 0), Some(rail(Shape::AscendingNorth))),
            [
                Some(BlockPos::new(0, 63, 0)),
                Some(BlockPos::new(0, 64, -1))
            ]
        );
    }

    fn run_ticks(
        cart: &mut Minecart,
        block: BlockLookup,
        ticks: usize,
        mut each: impl FnMut(&Minecart),
    ) {
        use rand::SeedableRng;
        use tuff_core::world::poi::PoiStore;

        let shared = tuff_test_support::bare_shared();
        let light = |_: BlockPos| Some((15, 0));
        let biome = |_: BlockPos| Some("minecraft:plains");
        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        let mut budget = 10_000;
        let pois = PoiStore::default();
        let state = tuff_core::world::region_api::RegionStateLock::default();
        for tick in 0..ticks {
            let mut effects = Vec::new();
            let mut cx = TickContext {
                tick: tick as u64 + 1,
                dimension: tuff_core::world::dimension::DimensionId::for_tests(0),
                block,
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
                entities: &[],
                entity_grid: None,
                pistons: &[],
                parts: &[],
                shared: &shared,
                rng: &mut rng,
                effects: &mut effects,
                path_budget: &mut budget,
                pois: &pois,
                state: &state,
            };
            cart.tick_on_track(&mut cx);
            cart.first_tick = false;
            each(cart);
        }
    }

    /// A loop of track: flat, a hill up and down, a curve, a booster, a
    /// brake and a V, ending at a wall.
    fn test_track(pos: BlockPos) -> Option<BlockState> {
        let shaped = |block: Block, shape: Shape| block.default_state().with_shape(shape).unwrap();
        let powered = |on: bool| {
            shaped(Block::PoweredRail, Shape::NorthSouth)
                .with("powered", if on { "true" } else { "false" })
                .unwrap()
        };
        let (x, y, z) = (pos.x, pos.y, pos.z);
        let rail = match (x, z) {
            (0..=9, 0) if y == 64 => Some(rail(Shape::EastWest)),
            (10, 0) if y == 64 => Some(rail(Shape::AscendingEast)),
            (11..=14, 0) if y == 65 => Some(rail(Shape::EastWest)),
            (15, 0) if y == 64 => Some(rail(Shape::AscendingWest)),
            (16..=19, 0) if y == 64 => Some(rail(Shape::EastWest)),
            (20, 0) if y == 64 => Some(rail(Shape::SouthWest)),
            (20, 3) if y == 64 => Some(powered(true)),
            (20, 9) if y == 64 => Some(powered(false)),
            (20, 14) if y == 64 => Some(rail(Shape::AscendingNorth)),
            (20, 15) if y == 64 => Some(rail(Shape::AscendingSouth)),
            (20, 1..=20) if y == 64 => Some(rail(Shape::NorthSouth)),
            (20, 21) if y == 64 => Some(Block::Stone.default_state()),
            _ => None,
        };
        rail.or_else(|| {
            Some(if y < 64 || (y == 64 && (11..=14).contains(&x) && z == 0) {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        })
    }

    fn fold(hash: &mut u64, value: u64) {
        *hash = (*hash ^ value).wrapping_mul(0x0000_0100_0000_01b3);
    }

    fn trace(start: [f64; 3], velocity: [f64; 3], ticks: usize) -> u64 {
        let mut cart = cart_at(start, velocity);
        cart.first_tick = true;
        let mut hash = 0xcbf2_9ce4_8422_2325;
        run_ticks(&mut cart, &test_track, ticks, |cart| {
            for c in cart.base.position.into_iter().chain(cart.base.velocity) {
                fold(&mut hash, c.to_bits());
            }
            fold(&mut hash, u64::from(cart.base.yaw.to_bits()));
            fold(&mut hash, u64::from(cart.base.pitch.to_bits()));
            fold(&mut hash, u64::from(cart.flipped));
        });
        for step in &cart.steps {
            for c in step.position.into_iter().chain(step.velocity) {
                fold(&mut hash, c.to_bits());
            }
            fold(&mut hash, u64::from(step.weight.to_bits()));
        }
        hash
    }

    #[tuff_core::watchdog]
    #[test]
    fn track_runs_are_pinned_bit_for_bit() {
        let runs = [
            trace([0.5, 64.0625, 0.5], [0.8, 0.0, 0.0], 120),
            trace([2.5, 64.0625, 0.5], [0.15, 0.0, 0.0], 200),
            trace([18.5, 64.0625, 0.5], [-0.6, 0.0, 0.0], 120),
            trace([20.5, 64.0625, 5.5], [0.0, 0.0, 0.05], 200),
            trace([20.5, 64.0625, 12.5], [0.0, 0.0, 0.3], 200),
        ];
        assert_eq!(
            runs,
            [
                10869819209157855911,
                10346869009232159064,
                2708208204940685104,
                1412768943752169407,
                4243467656024676664
            ]
        );
    }

    /// Release timing of the track walk: `cargo test --release -p
    /// tuff-feature-builtin-vanilla-entities track_walk_timing -- --ignored
    /// --nocapture`.
    #[tuff_core::watchdog]
    #[test]
    #[ignore]
    fn track_walk_timing() {
        let started = std::time::Instant::now();
        let mut sink = 0u64;
        for i in 0..400 {
            sink ^= trace(
                [0.5, 64.0625, 0.5],
                [0.4 + f64::from(i % 5) * 0.1, 0.0, 0.0],
                150,
            );
        }
        println!("track walk: {:?} ({sink:x})", started.elapsed());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_cart_over_unknown_terrain_leaves_the_track_instead_of_halting() {
        use rand::SeedableRng;
        use tuff_core::world::poi::PoiStore;

        let mut cart = cart_at([4.5, 64.0, 7.5], [0.4, 0.0, 0.0]);
        let shared = tuff_test_support::bare_shared();
        let block = |_: BlockPos| None;
        let light = |_: BlockPos| Some((15, 0));
        let biome = |_: BlockPos| Some("minecraft:plains");
        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        let mut effects = Vec::new();
        let mut budget = 10_000;
        let pois = PoiStore::default();
        let mut cx = TickContext {
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
            entities: &[],
            entity_grid: None,
            pistons: &[],
            parts: &[],
            shared: &shared,
            rng: &mut rng,
            effects: &mut effects,
            path_budget: &mut budget,
            pois: &pois,
            state: &tuff_core::world::region_api::RegionStateLock::default(),
        };
        cart.tick_on_track(&mut cx);
        assert!(cart.base.velocity[0] < 0.4, "{:?}", cart.base.velocity);
    }
}
