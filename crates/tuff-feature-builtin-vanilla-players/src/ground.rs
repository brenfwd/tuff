use tuff_core::world::block_contact;
use tuff_core::world::contact;
use tuff_core::world::fake::motion::{Body, FakeMotion, Held, MotionState};
use tuff_core::world::freezing;
use tuff_core::world::physics::{self, Aabb, Shape};
use tuff_core::world::session::mirror::ChunkMirror;
use tuff_vanilla_common::scaffolding;
use tuff_vanilla_data::Block;
use tuff_world_model::BlockPos;

const HALF_WIDTH: f64 = 0.3f32 as f64;
const STANDING: f64 = 1.8f32 as f64;
const CROUCHING: f64 = 1.5f32 as f64;
const SWIMMING: f64 = 0.6f32 as f64;
const FLUID_JUMP_THRESHOLD: f64 = 0.4;
const LAVA_CURRENT: f64 = 0.002_333_333_333_333_333_5;
const JUMP_COOLDOWN: u32 = 10;
const SPRINT_FOOD: i32 = 6;
const SPRINT_FACTOR: f32 = 1.3;
const MINOR_COLLISION_ANGLE: f64 = 0.139_626_339_077_949_52;

/// Walking, sprinting, sneaking, jumping and falling on solid ground.
pub struct GroundMotion;

/// What a client carries into a tick beyond its keys: gear, effects and
/// the entities it collides with. The default is a bare player.
#[derive(Debug, Clone, PartialEq)]
pub struct Rig {
    /// The caller sets sprinting and gliding itself, by command, so the
    /// sprint key is the sprint state and gliding never starts or ends
    /// on its own.
    pub driven: bool,
    /// Using an item, which slows the input.
    pub item_slow: bool,
    /// Scales every walking push.
    pub boost: f64,
    /// Speed effect multiplier, on the ground only.
    pub effect: f64,
    /// Dolphin's Grace, which keeps the swimmer gliding.
    pub dolphins: bool,
    /// The Levitation amplifier.
    pub levitation: Option<u8>,
    /// Wears leather boots.
    pub walker: bool,
    /// Under the Weaving effect.
    pub weaving: bool,
    /// Boxes of entities the body collides with.
    pub solids: Vec<Aabb>,
}

impl Default for Rig {
    fn default() -> Self {
        Rig {
            driven: false,
            item_slow: false,
            boost: 1.0,
            effect: 1.0,
            dolphins: false,
            levitation: None,
            walker: false,
            weaving: false,
            solids: Vec::new(),
        }
    }
}

fn sin_cos(yaw: f32) -> (f64, f64) {
    let radians = f64::from(yaw * (std::f64::consts::PI / 180.0) as f32);
    (
        f64::from(tuff_core::mth::sin(radians)),
        f64::from(tuff_core::mth::cos(radians)),
    )
}

fn box_at(position: [f64; 3], height: f64) -> Aabb {
    [
        position[0] - HALF_WIDTH,
        position[1],
        position[2] - HALF_WIDTH,
        position[0] + HALF_WIDTH,
        position[1] + height,
        position[2] + HALF_WIDTH,
    ]
}

fn shape(state: &MotionState, rig: &Rig, sneak: bool, world: &ChunkMirror, pos: BlockPos) -> Shape {
    match world.block_at(pos) {
        Some(s) if s.block() == Block::PowderSnow => {
            freezing::powder_snow_boxes(pos, state.position[1], state.fall, rig.walker).into()
        }
        Some(s) if s.block() == Block::Scaffolding => {
            scaffolding::collision_boxes(s, pos, state.position[1], sneak).into()
        }
        other => Shape::collision(other, pos),
    }
}

fn blocked(rig: &Rig, area: Aabb) -> bool {
    rig.solids.iter().any(|b| physics::intersects(*b, area))
}

fn movement_of(world: &ChunkMirror, pos: BlockPos) -> tuff_vanilla_data::entity::BlockMovement {
    world
        .block_at(pos)
        .unwrap_or(Block::Air.default_state())
        .block()
        .movement()
}

fn on_pos(state: &MotionState, world: &ChunkMirror, offset: f32) -> BlockPos {
    block_contact::on_pos(state.position, state.supporting, offset, &|p| {
        world.block_at(p)
    })
}

fn height(state: &MotionState) -> f64 {
    if state.swim_pose || state.gliding {
        SWIMMING
    } else if state.crouched {
        CROUCHING
    } else {
        STANDING
    }
}

fn eye_height(state: &MotionState) -> f64 {
    if state.swim_pose || state.gliding {
        0.4
    } else if state.crouched {
        f64::from(1.27f32)
    } else {
        f64::from(1.62f32)
    }
}

fn fits(state: &MotionState, rig: &Rig, sneak: bool, world: &ChunkMirror, height: f64) -> bool {
    let area = physics::inflate(box_at(state.position, height), [-1.0e-7; 3]);
    physics::colliders_shaped(area, &|pos| shape(state, rig, sneak, world, pos)).is_empty()
        && !blocked(rig, area)
}

fn can_fall(
    state: &MotionState,
    rig: &Rig,
    world: &ChunkMirror,
    dx: f64,
    dz: f64,
    drop: f64,
) -> bool {
    let b = box_at(state.position, height(state));
    let area = [
        b[0] + 1.0e-7 + dx,
        b[1] - drop - 1.0e-7,
        b[2] + 1.0e-7 + dz,
        b[3] - 1.0e-7 + dx,
        b[1],
        b[5] - 1.0e-7 + dz,
    ];
    physics::colliders_shaped(area, &|pos| shape(state, rig, true, world, pos)).is_empty()
        && !blocked(rig, area)
}

/// A sneaking body does not walk off an edge it could fall from.
fn back_off(
    state: &MotionState,
    rig: &Rig,
    world: &ChunkMirror,
    delta: [f64; 3],
    step: f64,
) -> [f64; 3] {
    const EDGE_STEP: f64 = 0.05;
    if delta[1] > 0.0 {
        return delta;
    }
    let supported = state.on_ground
        || (state.fall < step && !can_fall(state, rig, world, 0.0, 0.0, step - state.fall));
    if !supported {
        return delta;
    }
    let shrink = |v: f64| {
        if v.abs() <= EDGE_STEP {
            0.0
        } else {
            v - v.signum() * EDGE_STEP
        }
    };
    let mut horizontal = [delta[0], delta[2]];
    for axes in [[true, false], [false, true], [true, true]] {
        loop {
            let settled = (0..2).any(|i| axes[i] && horizontal[i] == 0.0);
            let probe = [0, 1].map(|i| if axes[i] { horizontal[i] } else { 0.0 });
            if settled || !can_fall(state, rig, world, probe[0], probe[1], step) {
                break;
            }
            for i in 0..2 {
                if axes[i] {
                    horizontal[i] = shrink(horizontal[i]);
                }
            }
        }
    }
    [horizontal[0], delta[1], horizontal[1]]
}

/// The keys as a push of `speed`, damped, slowed and stretched toward
/// the square, turned to face `yaw`.
fn push(held: &Held, rig: &Rig, slow: bool, speed: f64) -> [f64; 2] {
    let speed = speed * rig.boost;
    let strafe = f64::from(held.strafe.signum());
    let forward = f64::from(held.forward.signum());
    let len = strafe.hypot(forward);
    if len == 0.0 {
        return [0.0, 0.0];
    }
    let mut scale = 0.98;
    if rig.item_slow {
        scale *= 0.2;
    }
    if slow {
        scale *= 0.3;
    }
    let (ux, uz) = (strafe / len, forward / len);
    let tan = ux.abs().min(uz.abs()) / ux.abs().max(uz.abs());
    let m = (scale * (1.0 + tan * tan).sqrt()).min(1.0) * speed;
    let (sin, cos) = sin_cos(held.yaw);
    let (x, z) = (ux * m, uz * m);
    [x * cos - z * sin, z * cos + x * sin]
}

/// Whether the collision only grazed along the way the keys point.
fn minor_collision(held: &Held, wanted: [f64; 3]) -> bool {
    let (sin, cos) = sin_cos(held.yaw);
    let xxa = f64::from(held.strafe.signum());
    let zza = f64::from(held.forward.signum());
    let desired = [xxa * cos - zza * sin, zza * cos + xxa * sin];
    let desired_sq = desired[0].powi(2) + desired[1].powi(2);
    let wanted_sq = wanted[0].powi(2) + wanted[2].powi(2);
    if desired_sq < 9.999_999_747_378_752e-6 || wanted_sq < 9.999_999_747_378_752e-6 {
        return false;
    }
    let dot = desired[0] * wanted[0] + desired[1] * wanted[2];
    (dot / (desired_sq * wanted_sq).sqrt()).acos() < MINOR_COLLISION_ANGLE
}

fn leaves_support(
    state: &MotionState,
    held: &Held,
    body: &Body,
    rig: &Rig,
    world: &ChunkMirror,
    in_lava: bool,
) -> bool {
    if !held.jump_at_edge
        || held.jump
        || held.forward <= 0
        || !state.on_ground
        || state.gliding
        || state.in_water
        || in_lava
        || state.no_jump != 0
    {
        return false;
    }
    let mut ahead = state.clone();
    travel_in_air(&mut ahead, held, body, rig, world);
    can_fall(
        &ahead,
        rig,
        world,
        0.0,
        0.0,
        f64::from(body.movement.step_height),
    )
}

fn travel(
    state: &mut MotionState,
    held: &Held,
    body: &Body,
    rig: &Rig,
    world: &ChunkMirror,
) -> bool {
    let mut wanted = state.velocity;
    if state.stuck.iter().map(|v| v * v).sum::<f64>() > 1.0e-7 {
        for axis in 0..3 {
            wanted[axis] *= state.stuck[axis];
        }
        state.velocity = [0.0; 3];
        state.stuck = [0.0; 3];
    }
    if held.sneak {
        wanted = back_off(
            state,
            rig,
            world,
            wanted,
            f64::from(body.movement.step_height),
        );
    }
    let moved = physics::collide_with(
        box_at(state.position, height(state)),
        wanted,
        body.movement.step_height,
        state.on_ground,
        &|pos| shape(state, rig, held.sneak, world, pos),
        &rig.solids,
    );
    let moved_sq: f64 = moved.iter().map(|v| v * v).sum();
    let wanted_sq: f64 = wanted.iter().map(|v| v * v).sum();
    if moved_sq > 1.0e-7 || wanted_sq - moved_sq < 1.0e-7 {
        for axis in 0..3 {
            state.position[axis] += moved[axis];
        }
    }
    let x_hit = (moved[0] - wanted[0]).abs() >= 1.0e-5;
    let z_hit = (moved[2] - wanted[2]).abs() >= 1.0e-5;
    let y_hit = moved[1] != wanted[1];
    state.on_ground = y_hit && wanted[1] < 0.0;
    state.collided = (x_hit || z_hit) && !minor_collision(held, wanted);
    state.supporting = if state.on_ground {
        let here = box_at(state.position, height(state));
        let lookup = |p| world.block_at(p);
        physics::supporting_block(here, &lookup).or_else(|| {
            physics::supporting_block(physics::offset(here, [-moved[0], 0.0, -moved[2]]), &lookup)
        })
    } else {
        None
    };
    if !state.in_water {
        refresh_fluids(state, body, world);
    }
    if state.on_ground {
        state.fall = 0.0;
    } else if moved[1] < 0.0 && !state.in_water {
        state.fall -= moved[1];
    }
    if x_hit {
        state.velocity[0] = 0.0;
    }
    if z_hit {
        state.velocity[2] = 0.0;
    }
    if y_hit {
        let current = state.velocity[1];
        let below = world.block_at(on_pos(state, world, 0.2));
        let bounce = if state.on_ground && !held.sneak && -current > 0.08 {
            below.map_or(0.0, |b| f64::from(b.block().movement().bounce_restitution))
        } else {
            0.0
        };
        state.velocity[1] = if bounce > 0.0 {
            let portion = moved[1] / current;
            (portion * 0.08 - current) * (1.0 + portion * (f64::from(0.98f32) - 1.0)) * bounce
        } else {
            0.0
        };
    }
    let here = physics::block_pos(state.position);
    let factor_here = movement_of(world, here).speed_factor;
    let in_water_block = world
        .block_at(here)
        .is_some_and(|s| matches!(s.block(), Block::Water | Block::BubbleColumn));
    let factor = if in_water_block || factor_here != 1.0 {
        factor_here
    } else {
        movement_of(world, on_pos(state, world, 0.500_001)).speed_factor
    };
    state.velocity[0] *= f64::from(factor);
    state.velocity[2] *= f64::from(factor);
    x_hit || z_hit
}

fn refresh_fluids(state: &mut MotionState, body: &Body, world: &ChunkMirror) {
    let lookup = |p: BlockPos| world.block_at(p);
    let bounds = box_at(state.position, height(state));
    let sub = contact::submersion(bounds, lookup, |p| body.fluids.flow(&lookup, p));
    if sub.water > 0.0 {
        state.fall = 0.0;
        sub.water_current
            .push_player(&mut state.velocity, contact::WATER_CURRENT_SCALE);
    }
    if sub.lava > 0.0 {
        sub.lava_current
            .push_player(&mut state.velocity, LAVA_CURRENT);
    }
    state.in_water = sub.water > 0.0;
    let eye = [
        state.position[0],
        state.position[1] + eye_height(state),
        state.position[2],
    ];
    state.eyes_in_water = contact::eyes_in(eye, tuff_vanilla_data::tag::fluid::key::WATER, lookup);
}

fn suffocates_at(world: &ChunkMirror, pos: BlockPos, body: Aabb) -> bool {
    let area = [
        f64::from(pos.x) + 1.0e-7,
        body[1] + 1.0e-7,
        f64::from(pos.z) + 1.0e-7,
        f64::from(pos.x) + 1.0 - 1.0e-7,
        body[4] - 1.0e-7,
        f64::from(pos.z) + 1.0 - 1.0e-7,
    ];
    (area[1].floor() as i32..=area[4].floor() as i32).any(|y| {
        let cell = BlockPos::new(pos.x, y, pos.z);
        world.block_at(cell).is_some_and(|state| {
            state.is_suffocating()
                && physics::block_boxes(Some(state), cell).any(|b| physics::intersects(b, area))
        })
    })
}

/// Pushes out of a suffocating block toward its nearest free side, from
/// four probes 0.35 widths from the centre.
fn move_towards_closest_space(state: &mut MotionState, world: &ChunkMirror) {
    let body = box_at(state.position, height(state));
    let reach = f64::from(0.6f32) * 0.35;
    for (sx, sz) in [(-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0), (1.0, 1.0)] {
        let (x, z) = (
            state.position[0] + sx * reach,
            state.position[2] + sz * reach,
        );
        let pos = physics::block_pos([x, state.position[1], z]);
        if !suffocates_at(world, pos, body) {
            continue;
        }
        let (xd, zd) = (x - f64::from(pos.x), z - f64::from(pos.z));
        let sides = [
            (BlockPos::new(pos.x - 1, pos.y, pos.z), xd, 0, -0.1),
            (BlockPos::new(pos.x + 1, pos.y, pos.z), 1.0 - xd, 0, 0.1),
            (BlockPos::new(pos.x, pos.y, pos.z - 1), zd, 2, -0.1),
            (BlockPos::new(pos.x, pos.y, pos.z + 1), 1.0 - zd, 2, 0.1),
        ];
        let mut closest = f64::MAX;
        let mut pushed = None;
        for (side, distance, axis, speed) in sides {
            if distance < closest && !suffocates_at(world, side, body) {
                closest = distance;
                pushed = Some((axis, speed));
            }
        }
        if let Some((axis, speed)) = pushed {
            state.velocity[axis] = speed;
        }
    }
}

fn update_sprint(state: &mut MotionState, held: &Held, body: &Body) {
    let possible = body.food > SPRINT_FOOD;
    let shallow = state.in_water && !state.eyes_in_water;
    let forward = held.forward > 0;
    if !state.sprinting && forward && possible && !shallow && !state.crouched && held.sprint {
        state.sprinting = true;
    }
    if state.sprinting {
        let keep = if state.swimming {
            possible && state.in_water && (forward || state.on_ground || held.sneak)
        } else {
            possible && !shallow && forward && !state.collided
        };
        state.sprinting = keep;
    }
}

fn update_pose(state: &mut MotionState, rig: &Rig, world: &ChunkMirror, held: &Held) {
    if !fits(state, rig, held.sneak, world, SWIMMING) {
        return;
    }
    let wanted = if state.swimming || state.gliding {
        SWIMMING
    } else if held.sneak {
        CROUCHING
    } else {
        STANDING
    };
    let pose = if fits(state, rig, held.sneak, world, wanted) {
        wanted
    } else if fits(state, rig, held.sneak, world, CROUCHING) {
        CROUCHING
    } else {
        SWIMMING
    };
    state.swim_pose = pose == SWIMMING && !state.gliding;
    state.crouched = pose == CROUCHING;
}

fn is_water(world: &ChunkMirror, pos: BlockPos) -> bool {
    world.block_at(pos).is_some_and(|b| {
        tuff_datapack::tags::is(
            tuff_vanilla_data::tag::fluid::key::WATER,
            b.fluid_state().fluid(),
        )
    })
}

fn climbable(state: &MotionState, world: &ChunkMirror) -> bool {
    tuff_core::world::player::on_climbable(state.position, |p| world.block_at(p))
}

/// Slow, steady sinking while not sprinting, snapping to a tiny drift
/// when the next fall would be almost nothing.
fn settle_in_fluid(velocity: &mut [f64; 3], gravity: f64, was_falling: bool, sprinting: bool) {
    if gravity == 0.0 || sprinting {
        return;
    }
    let sunk = velocity[1] - gravity / 16.0;
    let near_rest = sunk.abs() < 0.003 && (velocity[1] - 0.005).abs() >= 0.003;
    velocity[1] = if was_falling && near_rest {
        -0.003
    } else {
        sunk
    };
}

fn jump_out_of_fluid(
    state: &mut MotionState,
    rig: &Rig,
    sneak: bool,
    world: &ChunkMirror,
    hit: bool,
    old_y: f64,
) {
    if !hit {
        return;
    }
    let v = state.velocity;
    let by = [
        v[0],
        v[1] + f64::from(0.6f32) - state.position[1] + old_y,
        v[2],
    ];
    let area = physics::offset(box_at(state.position, height(state)), by);
    let liquid = (area[0].floor() as i32..area[3].ceil() as i32).any(|x| {
        (area[1].floor() as i32..area[4].ceil() as i32).any(|y| {
            (area[2].floor() as i32..area[5].ceil() as i32).any(|z| {
                world
                    .block_at(BlockPos::new(x, y, z))
                    .is_some_and(|s| !s.fluid_state().is_empty())
            })
        })
    });
    if physics::colliders_shaped(area, &|pos| shape(state, rig, sneak, world, pos)).is_empty()
        && !blocked(rig, area)
        && !liquid
    {
        state.velocity[1] = f64::from(0.3f32);
    }
}

/// How one tick of swimming or wading pushes and drags a body.
struct Stroke {
    speed: f64,
    drag: [f64; 3],
    settle: bool,
    sink: f64,
    climbs: bool,
}

fn water_stroke(state: &MotionState, body: &Body, rig: &Rig) -> Stroke {
    let base_drag: f32 = if state.sprinting { 0.9 } else { 0.8 };
    let mut grip = body.movement.water_efficiency;
    if !state.on_ground {
        grip *= 0.5;
    }
    let (mut drag, speed) = if grip > 0.0 {
        let sprint_scale = if state.sprinting { SPRINT_FACTOR } else { 1.0 };
        (
            base_drag + (0.546_000_06 - base_drag) * grip,
            0.02 + (body.movement.speed * sprint_scale - 0.02) * grip,
        )
    } else {
        (base_drag, 0.02f32)
    };
    if rig.dolphins {
        drag = 0.96;
    }
    let drag = f64::from(drag);
    Stroke {
        speed: f64::from(speed),
        drag: [drag, f64::from(0.8f32), drag],
        settle: true,
        sink: 0.0,
        climbs: true,
    }
}

fn lava_stroke(lava_height: f64) -> Stroke {
    let shallow = lava_height <= FLUID_JUMP_THRESHOLD;
    Stroke {
        speed: f64::from(0.02f32),
        drag: [0.5, if shallow { f64::from(0.8f32) } else { 0.5 }, 0.5],
        settle: shallow,
        sink: 0.25,
        climbs: false,
    }
}

fn swim(
    state: &mut MotionState,
    held: &Held,
    body: &Body,
    rig: &Rig,
    world: &ChunkMirror,
    stroke: Stroke,
) {
    let was_falling = state.velocity[1] <= 0.0;
    let start_y = state.position[1];
    let pushed = push(held, rig, state.slowed, stroke.speed);
    state.velocity[0] += pushed[0];
    state.velocity[2] += pushed[1];
    let hit = travel(state, held, body, rig, world);
    if stroke.climbs && hit && climbable(state, world) {
        state.velocity[1] = 0.2;
    }
    for (v, d) in state.velocity.iter_mut().zip(stroke.drag) {
        *v *= d;
    }
    let gravity = body.movement.gravity;
    if stroke.settle {
        settle_in_fluid(&mut state.velocity, gravity, was_falling, state.sprinting);
    }
    if stroke.sink != 0.0 && gravity != 0.0 {
        state.velocity[1] -= gravity * stroke.sink;
    }
    jump_out_of_fluid(state, rig, held.sneak, world, hit, start_y);
}

fn travel_in_air(
    state: &mut MotionState,
    held: &Held,
    body: &Body,
    rig: &Rig,
    world: &ChunkMirror,
) {
    let climbing = climbable(state, world);
    let sprint_scale = if state.sprinting { SPRINT_FACTOR } else { 1.0 };
    let mut friction: f32 = 1.0;
    let speed = if state.on_ground {
        friction = movement_of(world, on_pos(state, world, 0.500_001)).friction;
        body.movement.speed
            * sprint_scale
            * rig.effect as f32
            * (0.21600002 / (friction * friction * friction))
    } else {
        0.02 * sprint_scale
    };
    let pushed = push(held, rig, state.slowed, f64::from(speed));
    state.velocity[0] += pushed[0];
    state.velocity[2] += pushed[1];
    if climbing {
        state.fall = 0.0;
        state.velocity[0] = state.velocity[0].clamp(-0.15, 0.15);
        state.velocity[2] = state.velocity[2].clamp(-0.15, 0.15);
        state.velocity[1] = state.velocity[1].max(-0.15);
        let in_scaffolding = world
            .block_at(physics::block_pos(state.position))
            .is_some_and(|b| b.block() == Block::Scaffolding);
        if state.velocity[1] < 0.0 && !in_scaffolding && held.sneak {
            state.velocity[1] = 0.0;
        }
    }
    let hit = travel(state, held, body, rig, world);
    if (hit || held.jump) && (climbable(state, world) || (state.in_snow && rig.walker)) {
        state.velocity[1] = 0.2;
    }
    let pulled = match rig.levitation {
        Some(amplifier) => {
            let y = state.velocity[1];
            y + (0.05 * f64::from(amplifier + 1) - y) * 0.2
        }
        None => state.velocity[1] - body.movement.gravity,
    };
    state.velocity[1] = pulled * f64::from(0.98f32);
    let drag = f64::from(friction * 0.91);
    state.velocity[0] *= drag;
    state.velocity[2] *= drag;
    if state.on_ground
        && !held.sneak
        && world
            .block_at(on_pos(state, world, 0.2))
            .is_some_and(|b| b.block() == Block::SlimeBlock)
        && state.velocity[1].abs() < 0.1
    {
        let scale = 0.4 + state.velocity[1].abs() * 0.2;
        state.velocity[0] *= scale;
        state.velocity[2] *= scale;
    }
}

fn travel_gliding(
    state: &mut MotionState,
    held: &Held,
    body: &Body,
    rig: &Rig,
    world: &ChunkMirror,
) {
    if climbable(state, world) {
        travel_in_air(state, held, body, rig, world);
        state.gliding = false;
        return;
    }
    if body.boosting {
        let look = tuff_core::world::raycast::view_vector(held.yaw, held.pitch);
        state.velocity = tuff_core::world::player::firework_boost(state.velocity, look);
    }
    state.velocity = tuff_core::world::player::glide_step(
        state.velocity,
        held.yaw,
        held.pitch,
        body.movement.gravity,
    );
    travel(state, held, body, rig, world);
}

fn jump_from_ground(state: &mut MotionState, held: &Held, body: &Body, world: &ChunkMirror) {
    let here = movement_of(world, physics::block_pos(state.position)).jump_factor;
    let factor = if here == 1.0 {
        movement_of(world, on_pos(state, world, 0.500_001)).jump_factor
    } else {
        here
    };
    state.velocity[1] = state.velocity[1].max(f64::from(body.movement.jump_strength * factor));
    if state.sprinting {
        let (sin, cos) = sin_cos(held.yaw);
        state.velocity[0] -= sin * 0.2;
        state.velocity[2] += cos * 0.2;
    }
    state.no_jump = JUMP_COOLDOWN;
}

impl FakeMotion for GroundMotion {
    fn tick(&self, state: &mut MotionState, held: &Held, body: &Body, world: &ChunkMirror) {
        self.advance(state, held, body, &Rig::default(), world);
    }
}

fn honey_slide(state: &mut MotionState, body: &Body) {
    let gravity = body.movement.gravity;
    let old = state.velocity[1] / f64::from(0.98f32) + gravity;
    if state.on_ground || old >= -gravity {
        return;
    }
    if old < -0.13 {
        let k = -0.05 / old;
        state.velocity[0] *= k;
        state.velocity[2] *= k;
    }
    state.velocity[1] = (-0.05 - gravity) * f64::from(0.98f32);
    state.fall = 0.0;
}

fn in_powder_snow(previous: Aabb, now: Aabb, world: &ChunkMirror) -> bool {
    let swept = [
        previous[0].min(now[0]) + 1.0e-5,
        previous[1].min(now[1]) + 1.0e-5,
        previous[2].min(now[2]) + 1.0e-5,
        previous[3].max(now[3]) - 1.0e-5,
        previous[4].max(now[4]) - 1.0e-5,
        previous[5].max(now[5]) - 1.0e-5,
    ];
    (swept[0].floor() as i32..=swept[3].floor() as i32).any(|x| {
        (swept[1].floor() as i32..=swept[4].floor() as i32).any(|y| {
            (swept[2].floor() as i32..=swept[5].floor() as i32).any(|z| {
                world
                    .block_at(BlockPos::new(x, y, z))
                    .is_some_and(|b| b.block() == Block::PowderSnow)
            })
        })
    })
}

impl GroundMotion {
    /// One client tick of `state` under `held`, carrying `rig`.
    pub fn advance(
        &self,
        state: &mut MotionState,
        held: &Held,
        body: &Body,
        rig: &Rig,
        world: &ChunkMirror,
    ) {
        let probe = physics::inflate(box_at(state.position, STANDING), [1.0; 3]);
        if world.touches_unknown(probe) {
            return;
        }
        let lookup = |p: BlockPos| world.block_at(p);
        let bounds = box_at(state.position, height(state));
        let eye = [
            state.position[0],
            state.position[1] + eye_height(state),
            state.position[2],
        ];
        let sub = contact::submersion(bounds, lookup, |p| body.fluids.flow(&lookup, p));
        let in_lava = sub.lava > 0.0;
        state.in_water = sub.water > 0.0;
        let eyes_were_in = state.eyes_in_water;
        state.eyes_in_water =
            contact::eyes_in(eye, tuff_vanilla_data::tag::fluid::key::WATER, lookup);
        if state.in_water {
            state.fall = 0.0;
            sub.water_current
                .push_player(&mut state.velocity, contact::WATER_CURRENT_SCALE);
        }
        if in_lava {
            state.fall *= 0.5;
            sub.lava_current
                .push_player(&mut state.velocity, LAVA_CURRENT);
        }
        let feet_in_water = is_water(world, physics::block_pos(state.position));
        state.swimming = state.sprinting
            && state.in_water
            && (state.swimming || (eyes_were_in && feet_in_water));
        state.slowed = (!state.swimming
            && fits(state, rig, held.sneak, world, CROUCHING)
            && (state.sneaked || !fits(state, rig, held.sneak, world, STANDING)))
            || (state.swim_pose && !state.gliding && !state.in_water);
        move_towards_closest_space(state, world);
        state.no_jump = state.no_jump.saturating_sub(1);
        if rig.driven {
            state.sprinting = held.sprint;
        } else {
            update_sprint(state, held, body);
        }
        if state.in_water && held.sneak {
            state.velocity[1] -= f64::from(0.04f32);
        }
        if state.velocity[0].powi(2) + state.velocity[2].powi(2) < 9.0e-6 {
            state.velocity[0] = 0.0;
            state.velocity[2] = 0.0;
        }
        if state.velocity[1].abs() < 0.003 {
            state.velocity[1] = 0.0;
        }
        if state.gliding && state.on_ground && !rig.driven {
            state.gliding = false;
        }
        if held.jump
            && !state.jump_down
            && body.glider
            && !rig.driven
            && !state.on_ground
            && !state.gliding
            && !state.in_water
            && !in_lava
            && !climbable(state, world)
        {
            state.gliding = true;
        }
        state.jump_down = held.jump;
        if state.gliding && state.velocity[1] > -0.5 && state.fall > 1.0 {
            state.fall = 1.0;
        }
        let edge_jump = Held {
            jump: true,
            ..*held
        };
        let held = if leaves_support(state, held, body, rig, world, in_lava) {
            &edge_jump
        } else {
            held
        };
        if held.jump {
            let fluid_height = if in_lava { sub.lava } else { sub.water };
            let in_water_height = state.in_water && fluid_height > 0.0;
            let lift = (in_water_height
                && (!state.on_ground || fluid_height > FLUID_JUMP_THRESHOLD))
                || (in_lava && (!state.on_ground || sub.lava > FLUID_JUMP_THRESHOLD));
            if lift {
                state.velocity[1] += f64::from(0.04f32);
            } else if (state.on_ground || (in_water_height && fluid_height <= FLUID_JUMP_THRESHOLD))
                && state.no_jump == 0
            {
                jump_from_ground(state, held, body, world);
            }
        } else {
            state.no_jump = 0;
        }
        let previous = box_at(state.position, height(state));
        if state.swimming {
            let look_y = -f64::from(tuff_core::mth::sin(f64::from(
                held.pitch * (std::f64::consts::PI / 180.0) as f32,
            )));
            let at = BlockPos::new(
                state.position[0].floor() as i32,
                (state.position[1] + 1.0 - 0.1).floor() as i32,
                state.position[2].floor() as i32,
            );
            let fluid_there = world
                .block_at(at)
                .is_some_and(|b| !b.fluid_state().is_empty());
            if look_y <= 0.0 || held.jump || fluid_there {
                let scale = if look_y < -0.2 { 0.085 } else { 0.06 };
                state.velocity[1] += (look_y - state.velocity[1]) * scale;
            }
        }
        if state.in_water || in_lava {
            if state.in_water {
                swim(
                    state,
                    held,
                    body,
                    rig,
                    world,
                    water_stroke(state, body, rig),
                );
            } else {
                swim(state, held, body, rig, world, lava_stroke(sub.lava));
            }
        } else if state.gliding {
            travel_gliding(state, held, body, rig, world);
        } else {
            travel_in_air(state, held, body, rig, world);
        }
        let now = box_at(state.position, height(state));
        let touched = block_contact::swept_contact(
            tuff_vanilla_data::registry::EntityType::Player,
            previous,
            now,
            None,
            &lookup,
        );
        state.stuck = touched
            .stuck
            .map_or([0.0; 3], |s| s.multiplier(rig.weaving));
        if block_contact::bubble_column_push(now, &mut state.velocity, &lookup) {
            state.fall = 0.0;
        }
        if touched.honey_side.is_some() {
            honey_slide(state, body);
        }
        state.in_snow = in_powder_snow(previous, now, world);
        update_pose(state, rig, world, held);
        state.sneaked = held.sneak;
    }
}
