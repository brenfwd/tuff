use std::time::{Duration, Instant};

use tuff_core::protocol::play::{GameMode, Movement, Serverbound};
use tuff_core::world::physics::{Aabb, modified_friction};
use tuff_core::world::session::mirror::ChunkMirror;
use tuff_vanilla_data::fluid::FluidBehaviour;
use tuff_vanilla_data::tag;
use tuff_vanilla_data::{Block, BlockState, Item};
use tuff_world_model::BlockPos;

use super::budget::{BudgetLimits, PacketBudget};
use super::predict::{Momentum, Path, Regime};
use super::{Check, Facts, Fix, Flag};
use crate::trace::{Tags, Trace};

const TICK: Duration = Duration::from_millis(50);
const HALF_WIDTH: f64 = 0.3;
const WALK_LIMIT: f64 = 0.8;
const SLIPPERY_LIMIT: f64 = 1.5;
const SPEED_STEP: f64 = 0.25;
const JUMP_VELOCITY: f64 = 0.42;
const JUMP_BOOST_STEP: f64 = 0.1;
const GRAVITY: f64 = 0.08;
const DRAG: f64 = 0.98;
const VERTICAL_SLACK: f64 = 0.03;
const BOUNCE_SLACK: f64 = 0.2;
const STEP_HEIGHT: f64 = 0.6;
const CROUCH_HEIGHT: f64 = 1.5;
const STANDING_HEIGHT: f64 = 1.8;
const SWEEP_LIMIT: f64 = 16.0;
const CLIP_TOLERANCE: f64 = 0.05;
const FLUSH: f64 = 1.0e-3;
const TIMER_RESERVE: f64 = 60.0;
const TIMER_TOLERANCE: f64 = 6.0;
const TIMER_FLOOR: f64 = -200.0;
const PING_CREDIT_TICKS: f64 = 20.0;
const MAX_LAG: f64 = 4.0;
const LAG_WINDOW: u64 = 20;
const UNCONFIRMED_MOVES: u32 = 40;
const SUSTAINED_LIMIT: f64 = 0.5;
const GRACE_FACTOR: f64 = 1.5;
const HUGE_MOVE: f64 = 64.0;
const SNEAK_LIMIT: f64 = 0.2;
const SNEAK_SETTLE_MOVES: u32 = 4;
const EXCESS_TOLERANCE: f64 = 0.5;
const EXCESS_FLOOR: f64 = -0.5;
#[cfg(test)]
const USE_INPUT_SCALE: f64 = 0.2;
#[cfg(test)]
const GROUND_FRICTION: f64 = 0.6 * 0.91;
const AIR_FRICTION: f64 = 0.91;
const WALK_SPEED: f64 = 0.1;
const VANILLA_SPEED_STEP: f64 = 0.2;
const SNEAK_SCALE: f32 = 0.3;
const AIR_ACCEL: f64 = 0.02;
const SPRINT_FACTOR: f64 = 1.3;
const SPRINT_JUMP: f64 = 0.2;
const UNEXPLAINED_SLACK: f64 = 0.002;
const USE_SLACK: f64 = 0.001;
const SOUL_SAND_FACTOR: f64 = 0.4;
const BLOCK_FRICTION: f32 = 0.6;

/// What the client's own copy of the world says around a move.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Terrain {
    /// Every chunk the checks need was sent to the client.
    pub known: bool,
    /// The move may end on the ground: something solid lies right under
    /// the feet at the new height anywhere from the old position to the
    /// new one. The game settles vertical movement before horizontal, so a
    /// player stepping off an edge keeps its footing for that move, and a
    /// step up may keep the height over a slightly lower floor.
    pub supported: bool,
    /// Something solid lies right under the feet at the new position.
    pub underfoot: bool,
    /// Liquid, a climbable, a cobweb, a berry bush, honey, powder snow or
    /// a bubble column touches the body, which all change vertical
    /// movement.
    pub soft: bool,
    /// Ice under the feet.
    pub slippery: bool,
    /// Slime, a shelf mushroom or a bed under the feet, which bounce.
    pub bouncy: bool,
    /// The knee-height core of the body is inside a solid block.
    pub phased: bool,
    /// Whether the old position's core was inside a solid block.
    pub was_phased: bool,
    /// No axis ordering slides the body's core from the old position to the
    /// new one without meeting a block it was not already inside.
    pub clipped: bool,
    /// Water or a climbable holds the body, which stops a fall.
    pub medium: bool,
    /// Lava, fire, a cactus, magma or the like touches the body.
    pub hazard: bool,
    /// A block beside or above the body could stop a push.
    pub cramped: bool,
    /// Soul sand holds the feet, which slows the next move's momentum.
    pub sticky: bool,
    /// Friction of the block under the feet's centre.
    pub friction: f32,
    /// The highest friction among the blocks under the feet's box, where
    /// the move ended or started (the game may keep the start's support).
    pub grip: f32,
    /// The lowest friction among the same blocks.
    pub traction: f32,
}

impl Default for Terrain {
    fn default() -> Self {
        Self {
            known: false,
            supported: false,
            underfoot: false,
            soft: false,
            slippery: false,
            bouncy: false,
            phased: false,
            was_phased: false,
            clipped: false,
            medium: false,
            hazard: false,
            cramped: false,
            sticky: false,
            friction: BLOCK_FRICTION,
            grip: BLOCK_FRICTION,
            traction: BLOCK_FRICTION,
        }
    }
}

fn knee(position: [f64; 3]) -> Aabb {
    [
        position[0] - 0.1,
        position[1] + 0.3,
        position[2] - 0.1,
        position[0] + 0.1,
        position[1] + 0.5,
        position[2] + 0.1,
    ]
}

fn feet_probe(position: [f64; 3]) -> Aabb {
    [
        position[0] - HALF_WIDTH,
        position[1] - 0.05,
        position[2] - HALF_WIDTH,
        position[0] + HALF_WIDTH,
        position[1] + 0.001,
        position[2] + HALF_WIDTH,
    ]
}

/// The feet probe swept from `old` to `new` at the new height, a hair
/// wider so a block top flush with the box's side counts too.
fn feet_sweep(old: [f64; 3], new: [f64; 3]) -> Aabb {
    let [x0, y0, z0, x1, y1, z1] = feet_probe(new);
    [
        x0.min(old[0] - HALF_WIDTH) - FLUSH,
        y0,
        z0.min(old[2] - HALF_WIDTH) - FLUSH,
        x1.max(old[0] + HALF_WIDTH) + FLUSH,
        y1,
        z1.max(old[2] + HALF_WIDTH) + FLUSH,
    ]
}

fn behaviour(state: BlockState) -> Option<FluidBehaviour> {
    state.fluid_state().rules().map(|rules| rules.behaviour)
}

fn hurts(state: BlockState) -> bool {
    matches!(
        state.block(),
        Block::Fire
            | Block::SoulFire
            | Block::MagmaBlock
            | Block::Cactus
            | Block::SweetBerryBush
            | Block::WitherRose
            | Block::PowderSnow
    ) || (matches!(state.block(), Block::Campfire | Block::SoulCampfire)
        && state.get("lit") == Some("true"))
        || behaviour(state) == Some(FluidBehaviour::Lava)
}

fn holds(state: BlockState) -> bool {
    tuff_datapack::tags::is(tag::block::key::CLIMBABLE, state.block())
        || state.block() == Block::BubbleColumn
        || behaviour(state) == Some(FluidBehaviour::Water)
}

fn changes_vertical_movement(state: BlockState) -> bool {
    state.is_liquid()
        || state.get("waterlogged") == Some("true")
        || tuff_datapack::tags::is(tag::block::key::CLIMBABLE, state.block())
        || matches!(
            state.block(),
            Block::Cobweb
                | Block::SweetBerryBush
                | Block::HoneyBlock
                | Block::PowderSnow
                | Block::BubbleColumn
        )
}

/// Something solid for every body fills `area`: scaffolding and powder
/// snow only hold what stands on them, so a body in them is not inside.
fn inside_solid(mirror: &ChunkMirror, area: Aabb) -> bool {
    !tuff_core::world::physics::colliders_shaped(area, &|pos| match mirror.block_at(pos) {
        Some(state) if matches!(state.block(), Block::Scaffolding | Block::PowderSnow) => {
            tuff_core::world::physics::Shape::from(&[][..])
        }
        Some(state) => tuff_core::world::physics::Shape::collision(Some(state), pos),
        None => tuff_core::world::physics::Shape::from(&[][..]),
    })
    .is_empty()
}

fn core_shapes(mirror: &ChunkMirror, area: Aabb) -> Vec<Aabb> {
    tuff_core::world::physics::colliders_shaped(area, &|pos| match mirror.block_at(pos) {
        Some(state) if matches!(state.block(), Block::Scaffolding | Block::PowderSnow) => {
            tuff_core::world::physics::Shape::from(&[][..])
        }
        Some(state) => tuff_core::world::physics::Shape::collision(Some(state), pos),
        None => tuff_core::world::physics::Shape::from(&[][..]),
    })
}

fn clipped(mirror: &ChunkMirror, old: [f64; 3], new: [f64; 3], step_height: f64) -> bool {
    use tuff_core::world::physics::{collide_with_shapes, expand_towards};
    const INSET: f64 = 0.2;
    const ORDERS: [[usize; 3]; 6] = [
        [1, 0, 2],
        [1, 2, 0],
        [0, 1, 2],
        [2, 1, 0],
        [0, 2, 1],
        [2, 0, 1],
    ];
    let movement = [0, 1, 2].map(|i| new[i] - old[i]);
    let length_sq = movement.iter().map(|v| v * v).sum::<f64>();
    if !length_sq.is_finite() || length_sq > SWEEP_LIMIT * SWEEP_LIMIT {
        return false;
    }
    let half = HALF_WIDTH - INSET / 2.0;
    let core = [
        old[0] - half,
        old[1] + step_height + INSET / 2.0,
        old[2] - half,
        old[0] + half,
        old[1] + CROUCH_HEIGHT - INSET / 2.0,
        old[2] + half,
    ];
    let standing = |at: [f64; 3]| {
        [
            at[0] - half,
            at[1] + step_height + INSET / 2.0,
            at[2] - half,
            at[0] + half,
            at[1] + STANDING_HEIGHT - INSET / 2.0,
            at[2] + half,
        ]
    };
    if !core_shapes(mirror, standing(old)).is_empty()
        || !core_shapes(mirror, standing(new)).is_empty()
    {
        return false;
    }
    let shapes = core_shapes(mirror, expand_towards(core, movement));
    if shapes.is_empty() {
        return false;
    }
    ORDERS.iter().all(|order| {
        let mut at = core;
        order.iter().any(|&axis| {
            let mut step = [0.0; 3];
            step[axis] = movement[axis];
            let done = collide_with_shapes(step, at, &shapes);
            at[axis] += done[axis];
            at[axis + 3] += done[axis];
            (movement[axis] - done[axis]).abs() > CLIP_TOLERANCE
        })
    })
}

impl Terrain {
    pub fn probe(mirror: &ChunkMirror, old: [f64; 3], new: [f64; 3], step_height: f64) -> Terrain {
        let reach = [
            new[0] - 1.0,
            new[1] - 1.5,
            new[2] - 1.0,
            new[0] + 1.0,
            new[1] + 2.0,
            new[2] + 1.0,
        ];
        if mirror.touches_unknown(reach) {
            return Terrain::default();
        }
        let floor = |v: f64| v.floor() as i32;
        let body = [
            new[0] - HALF_WIDTH - 0.2,
            new[1] - 0.6,
            new[2] - HALF_WIDTH - 0.2,
            new[0] + HALF_WIDTH + 0.2,
            new[1] + 1.9,
            new[2] + HALF_WIDTH + 0.2,
        ];
        let mut soft = false;
        for x in floor(body[0])..=floor(body[3]) {
            for y in floor(body[1])..=floor(body[4]) {
                for z in floor(body[2])..=floor(body[5]) {
                    if let Some(state) = mirror.block_at(BlockPos::new(x, y, z)) {
                        soft |= changes_vertical_movement(state);
                    }
                }
            }
        }
        let before = [
            old[0] - HALF_WIDTH - 0.2,
            old[1] - 0.6,
            old[2] - HALF_WIDTH - 0.2,
            old[0] + HALF_WIDTH + 0.2,
            old[1] + 1.9,
            old[2] + HALF_WIDTH + 0.2,
        ];
        for x in floor(before[0])..=floor(before[3]) {
            for y in floor(before[1])..=floor(before[4]) {
                for z in floor(before[2])..=floor(before[5]) {
                    soft |= mirror
                        .block_at(BlockPos::new(x, y, z))
                        .is_some_and(|state| {
                            matches!(state.block(), Block::PowderSnow | Block::Scaffolding)
                        });
                }
            }
        }
        let mut slippery = false;
        let mut bouncy = false;
        for x in floor(new[0] - HALF_WIDTH)..=floor(new[0] + HALF_WIDTH) {
            for z in floor(new[2] - HALF_WIDTH)..=floor(new[2] + HALF_WIDTH) {
                for y in [floor(new[1] - 0.5), floor(new[1] - 1.5)] {
                    if let Some(state) = mirror.block_at(BlockPos::new(x, y, z)) {
                        slippery |= matches!(
                            state.block(),
                            Block::Ice | Block::PackedIce | Block::BlueIce | Block::FrostedIce
                        );
                        bouncy |= matches!(state.block(), Block::SlimeBlock | Block::ShelfMushroom)
                            || state.block().bed().is_some();
                    }
                }
            }
        }
        let below = floor(new[1] - 0.500_001);
        let friction_of = |x: i32, z: i32| {
            mirror
                .block_at(BlockPos::new(x, below, z))
                .map_or(BLOCK_FRICTION, |state| state.block().movement().friction)
        };
        let friction = friction_of(floor(new[0]), floor(new[2]));
        let mut grip = friction;
        let mut traction = friction;
        for at in [new, old] {
            for x in floor(at[0] - HALF_WIDTH)..=floor(at[0] + HALF_WIDTH) {
                for z in floor(at[2] - HALF_WIDTH)..=floor(at[2] + HALF_WIDTH) {
                    let here = friction_of(x, z);
                    grip = grip.max(here);
                    traction = traction.min(here);
                }
            }
        }
        let mut medium = false;
        let mut hazard = false;
        for x in floor(new[0] - HALF_WIDTH)..=floor(new[0] + HALF_WIDTH) {
            for y in floor(new[1] - 0.1)..=floor(new[1] + 1.8) {
                for z in floor(new[2] - HALF_WIDTH)..=floor(new[2] + HALF_WIDTH) {
                    if let Some(state) = mirror.block_at(BlockPos::new(x, y, z)) {
                        medium |= holds(state);
                        hazard |= hurts(state);
                    }
                }
            }
        }
        let fresh = mirror.changed_recently([
            old[0].min(new[0]) - 1.0,
            old[1].min(new[1]),
            old[2].min(new[2]) - 1.0,
            old[0].max(new[0]) + 1.0,
            old[1].max(new[1]) + 2.0,
            old[2].max(new[2]) + 1.0,
        ]);
        let underfoot = !mirror.colliders(feet_probe(new)).is_empty();
        let sticky = [floor(new[1]), floor(new[1] - 0.500_000_1)]
            .into_iter()
            .any(|y| {
                mirror
                    .block_at(BlockPos::new(floor(new[0]), y, floor(new[2])))
                    .is_some_and(|state| state.block() == Block::SoulSand)
            });
        Terrain {
            sticky,
            friction,
            grip,
            traction,
            known: true,
            medium,
            hazard,
            cramped: !mirror
                .colliders([
                    new[0] - 1.0,
                    new[1] + 0.1,
                    new[2] - 1.0,
                    new[0] + 1.0,
                    new[1] + 2.6,
                    new[2] + 1.0,
                ])
                .is_empty(),
            supported: underfoot || !mirror.colliders(feet_sweep(old, new)).is_empty(),
            underfoot,
            soft,
            slippery,
            bouncy,
            phased: !fresh && inside_solid(mirror, knee(new)),
            was_phased: inside_solid(mirror, knee(old)),
            clipped: !fresh && clipped(mirror, old, new, step_height),
        }
    }
}

/// The prediction engine replayed this move exactly, which the speed and
/// air envelopes cannot improve on.
fn replayed(facts: &Facts) -> bool {
    facts
        .prediction
        .is_some_and(|p| p.tags.contains(Tags::PREDICTED))
}

fn exempt(facts: &Facts) -> bool {
    facts.instabuild
        || facts.may_fly
        || facts.flying
        || facts.riding
        || facts.game_mode == GameMode::Spectator
}

fn setback(facts: &Facts, check: &'static str, weight: f64) -> Option<Flag> {
    Some(Flag {
        check,
        weight,
        fix: Fix::Teleport(facts.position),
    })
}

/// Wall-clock movement budget. A client sends at most one position
/// packet per client tick of 50 ms, however slow or fast the server
/// ticks, so each packet costs one tick and elapsed time pays it back.
/// A reserve absorbs lag bursts, widened by the measured ping and by
/// how far the server's own ticking runs behind its target.
pub struct TimerBalance {
    balance: f64,
    last: Option<Instant>,
    window: Option<(u64, Instant)>,
    lag: f64,
    unconfirmed: u32,
    packets: PacketBudget,
}

impl Default for TimerBalance {
    fn default() -> Self {
        Self {
            balance: 0.0,
            last: None,
            window: None,
            lag: 1.0,
            unconfirmed: 0,
            packets: PacketBudget::new(BudgetLimits::MOVEMENT),
        }
    }
}

impl TimerBalance {
    fn measure_lag(&mut self, facts: &Facts) {
        let Some((tick, at)) = self.window else {
            self.window = Some((facts.tick, facts.now));
            return;
        };
        if facts.tick.saturating_sub(tick) < LAG_WINDOW {
            return;
        }
        let wall = facts.now.saturating_duration_since(at).as_secs_f64();
        let per_tick = wall / (facts.tick - tick) as f64;
        let expected = 1.0 / f64::from(facts.tick_rate.clamp(0.1, 20.0));
        self.lag = (per_tick / expected).clamp(1.0, MAX_LAG);
        self.window = Some((facts.tick, facts.now));
    }
}

impl Check for TimerBalance {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        let Serverbound::Move(movement) = packet else {
            return None;
        };
        if facts.riding || facts.confirming {
            return None;
        }
        if movement.position.is_none() {
            return (!facts.teleport_pending
                && self.packets.record_scaled(facts.now, self.lag).is_some())
            .then(|| setback(facts, "timer", 1.0))
            .flatten();
        }
        self.measure_lag(facts);
        if facts.teleport_pending {
            self.unconfirmed += 1;
            let patience = UNCONFIRMED_MOVES + (f64::from(facts.latency_ms) / 50.0) as u32;
            return if self.unconfirmed > patience {
                setback(facts, "timer", 1.0)
            } else {
                None
            };
        }
        self.unconfirmed = 0;
        let reserve = TIMER_RESERVE.mul_add(
            self.lag,
            (f64::from(facts.latency_ms) / 50.0).clamp(0.0, PING_CREDIT_TICKS),
        );
        let credit = match self.last {
            Some(last) => {
                facts.now.saturating_duration_since(last).as_secs_f64() / TICK.as_secs_f64()
            }
            None => reserve,
        };
        self.last = Some(facts.now);
        self.balance = (self.balance + credit - 1.0).clamp(TIMER_FLOOR, reserve);
        if self.balance < -TIMER_TOLERANCE * self.lag
            || self.packets.record_scaled(facts.now, self.lag).is_some()
        {
            setback(facts, "timer", 1.0)
        } else {
            None
        }
    }
}

/// Horizontal distance per move packet, and no-clip through solid
/// blocks. One packet is one client tick, so the limit does not depend
/// on the server's tick rate.
#[derive(Default)]
pub struct MoveEnvelope {
    trace: Option<Trace>,
    weak: f64,
}

impl Check for MoveEnvelope {
    fn trace(&self) -> Option<Trace> {
        self.trace
    }

    fn weak(&mut self) -> f64 {
        std::mem::take(&mut self.weak)
    }

    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        self.trace = None;
        self.weak = 0.0;
        let Serverbound::Move(movement) = packet else {
            return None;
        };
        let position = movement.position?;
        if facts.teleport_pending || facts.riding {
            return None;
        }
        let terrain = facts.terrain.unwrap_or_default();
        if !exempt(facts) && !facts.velocity {
            let boost = 1.0 + SPEED_STEP * facts.speed.map_or(0.0, |a| f64::from(a) + 1.0);
            let base = if terrain.slippery || !terrain.known {
                SLIPPERY_LIMIT
            } else {
                WALK_LIMIT
            };
            let limit =
                base * boost * if facts.grace { GRACE_FACTOR } else { 1.0 } + facts.push.horizontal;
            let h = (position[0] - facts.position[0]).hypot(position[2] - facts.position[2]);
            self.trace = Trace::new(
                "move_speed",
                limit,
                h,
                Tags::empty()
                    .with(Tags::SLIPPERY, terrain.slippery)
                    .with(Tags::UNKNOWN_TERRAIN, !terrain.known)
                    .with(Tags::GRACE, facts.grace)
                    .with(Tags::SPEED_EFFECT, facts.speed.is_some())
                    .with(Tags::PUSH, facts.push.horizontal > 0.0),
            );
            if h > HUGE_MOVE {
                return setback(facts, "move_speed", 2.0);
            }
            if h > limit && !replayed(facts) {
                return setback(facts, "move_speed", 1.0 + ((h - limit) / limit).min(4.0));
            }
            self.weak = crate::improbable::weak_weight(h, limit, 0.5);
        }
        if terrain.known
            && (terrain.clipped || terrain.phased && !terrain.was_phased)
            && !facts.velocity
            && !facts.riding
            && !facts.confirming
            && facts.game_mode != GameMode::Spectator
        {
            self.trace = Trace::new("phase", 0.0, 1.0, Tags::PHASE);
            return setback(facts, "phase", 1.0);
        }
        None
    }
}

/// Sustained horizontal speed. A single packet may be a sprint jump's
/// burst, but a client that keeps moving faster than any legitimate
/// state allows accumulates the surplus per packet; staying under the
/// limit pays it back. Ice, liquids, unknown terrain, velocity and
/// teleports reset the account.
#[derive(Default)]
pub struct SustainedSpeed {
    excess: f64,
    sneak_run: u32,
    trace: Option<Trace>,
}

impl Check for SustainedSpeed {
    fn trace(&self) -> Option<Trace> {
        self.trace
    }

    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        self.trace = None;
        let Serverbound::Move(movement) = packet else {
            return None;
        };
        let position = movement.position?;
        let terrain = facts.terrain.unwrap_or_default();
        if facts.teleport_pending
            || facts.riding
            || exempt(facts)
            || facts.velocity
            || !terrain.known
            || terrain.slippery
            || terrain.soft
        {
            self.excess = 0.0;
            return None;
        }
        let boost = 1.0 + SPEED_STEP * facts.speed.map_or(0.0, |a| f64::from(a) + 1.0);
        self.sneak_run = if facts.sneaking {
            self.sneak_run.saturating_add(1)
        } else {
            0
        };
        let base = if self.sneak_run > SNEAK_SETTLE_MOVES {
            SNEAK_LIMIT
        } else {
            SUSTAINED_LIMIT
        };
        let h = (position[0] - facts.position[0]).hypot(position[2] - facts.position[2]);
        if replayed(facts) {
            self.excess = self.excess.min(0.0);
            return None;
        }
        self.excess = (self.excess + h - base * boost - facts.push.horizontal).max(EXCESS_FLOOR);
        self.trace = Trace::new(
            "move_speed",
            base * boost + facts.push.horizontal,
            h,
            Tags::empty()
                .with(Tags::SNEAK, facts.sneaking)
                .with(Tags::SPEED_EFFECT, facts.speed.is_some())
                .with(Tags::PUSH, facts.push.horizontal > 0.0),
        );
        if self.excess > EXCESS_TOLERANCE {
            self.excess = 0.0;
            return setback(facts, "move_speed", 1.0);
        }
        None
    }
}

/// The input scale an item in use applies to walking, from its
/// `use_effects` speed multiplier (spears keep full speed).
pub(super) fn use_input_scale(item: Item) -> Option<f64> {
    let scale = item.use_effects().speed_multiplier;
    (scale < 1.0).then_some(f64::from(scale))
}

/// How much a block's friction scales walking acceleration on it: above
/// the default block's friction the game divides by the cube of the
/// friction relative to it; at or below it the acceleration is unscaled.
fn grip_scale(friction: f32) -> f64 {
    if friction > BLOCK_FRICTION {
        f64::from(0.216_000_02_f32 / (friction * friction * friction))
    } else {
        1.0
    }
}

/// Horizontal momentum across moves, for bounding how far a player whose
/// input an item in use scales down may move. The momentum is the
/// predictor's, so a step the client backed off at an edge or spread over
/// unreported ticks does not shrink it. The bound decays the last step by
/// the friction of the block it started on (ground only after a few
/// supported moves, air otherwise) and adds the longest scaled
/// acceleration. Abstains in liquids, on unknown terrain, or where
/// something else moved the player.
pub struct UseMomentum {
    momentum: Momentum,
    supported: [bool; 2],
    /// Friction under the feet at the last move: centre block, highest and
    /// lowest under the box.
    block: [f32; 3],
    sticky: bool,
    path: Option<Path>,
}

impl Default for UseMomentum {
    fn default() -> Self {
        Self {
            momentum: Momentum::default(),
            supported: [false; 2],
            block: [BLOCK_FRICTION; 3],
            sticky: false,
            path: None,
        }
    }
}

impl UseMomentum {
    /// Follows the keys and tick ends the client reports.
    pub fn observe(&mut self, packet: &Serverbound) {
        self.momentum.observe(packet);
    }

    /// How the predictor explained the last move.
    pub fn path(&self) -> Option<Path> {
        self.path
    }

    /// Records this move and returns the farthest it could go with the
    /// item use slowing the input, or `None` when it cannot tell. A move
    /// past the prediction carries only the predicted momentum on.
    pub fn allowed(
        &mut self,
        facts: &Facts,
        movement: &Movement,
        position: [f64; 3],
    ) -> Option<f64> {
        let terrain = facts.terrain.unwrap_or_default();
        let delta = [
            position[0] - facts.position[0],
            position[2] - facts.position[2],
        ];
        let [mut prev, mut prev2] = self.supported;
        if self.momentum.last_motion().is_none() {
            prev = terrain.supported;
            prev2 = terrain.supported;
            self.block = [terrain.friction, terrain.grip, terrain.traction];
        }
        let [_, grip, traction] = std::mem::replace(
            &mut self.block,
            [terrain.friction, terrain.grip, terrain.traction],
        )
        .map(|friction| modified_friction(friction, facts.body.friction_modifier));
        let drag = modified_friction(AIR_FRICTION as f32, facts.body.air_drag_modifier);
        self.supported = [terrain.supported, prev];
        let stuck = std::mem::replace(
            &mut self.sticky,
            terrain.sticky && facts.body.efficiency <= 0.0,
        );
        self.path = None;
        if facts.teleport_pending
            || facts.confirming
            || exempt(facts)
            || facts.velocity
            || !terrain.known
            || terrain.soft
        {
            self.momentum.reset();
            return None;
        }
        let slowed = facts
            .item_use
            .as_ref()
            .and_then(|used| use_input_scale(used.item));
        let sneaking = facts.sneaking || self.momentum.keys().sneaking();
        let amplifier = facts.speed.map_or(0.0, |a| f64::from(a) + 1.0);
        let sprint = if facts.sprinting { SPRINT_FACTOR } else { 1.0 };
        let air_speed = if facts.sprinting {
            AIR_ACCEL * SPRINT_FACTOR
        } else {
            AIR_ACCEL
        };
        let exact = Regime {
            friction: if prev { grip * drag } else { drag },
            speed: if prev {
                WALK_SPEED * (1.0 + VANILLA_SPEED_STEP * amplifier) * sprint * grip_scale(traction)
            } else {
                air_speed
            } as f32,
            item: slowed.unwrap_or(1.0) as f32,
            sneak: if sneaking { SNEAK_SCALE } else { 1.0 },
            yaw: movement.rotation.map_or(facts.rotation[0], |(yaw, _)| yaw),
            sprint_jump: facts.sprinting
                && prev
                && !terrain.supported
                && position[1] > facts.position[1],
            edge: sneaking && prev,
        };
        let boost = 1.0 + VANILLA_SPEED_STEP * amplifier;
        let lenient = Regime {
            speed: if prev {
                WALK_SPEED * boost * sprint * grip_scale(traction)
            } else if terrain.supported {
                WALK_SPEED * boost * sprint
            } else {
                air_speed * boost
            } as f32,
            sneak: 1.0,
            ..exact
        };
        let friction = f64::from(if terrain.supported && prev && prev2 {
            grip * drag
        } else {
            drag
        });
        let jump = if facts.sprinting && prev && !terrain.supported {
            SPRINT_JUMP
        } else {
            0.0
        };
        let bound = self.momentum.last_motion().map(|before| {
            before * friction * if stuck { SOUL_SAND_FACTOR } else { 1.0 }
                + lenient.max_accel()
                + jump
                + facts.push.horizontal
                + USE_SLACK
        });
        self.momentum.set_strict(slowed.is_some());
        let path = self
            .momentum
            .judge(delta, &exact, bound.unwrap_or(f64::INFINITY));
        self.path = Some(path);
        slowed?;
        match (path, self.momentum.predicted()) {
            (Path::Unexplained, Some(predicted)) => {
                bound.map(|b| b.min(predicted + UNEXPLAINED_SLACK))
            }
            _ => bound,
        }
    }
}

/// Rising or hovering without support. Each move's vertical velocity is
/// at most the bound, which follows the game's gravity and drag from the
/// last move: from a full jump's launch after standing, and from the
/// move's own climb when it rose less than the bound allowed (a ceiling
/// leaves no more speed than that). A move leaving the ground may instead
/// step up a block edge, which stops it. Anything that can legitimately
/// break that (flight, liquids, climbables, bounces, velocity, effects)
/// resets the bound instead.
pub struct AirBound {
    bound: f64,
    grounded: bool,
    fall_speed: f64,
    tags: Tags,
    trace: Option<Trace>,
}

impl Default for AirBound {
    fn default() -> Self {
        Self {
            bound: JUMP_VELOCITY,
            grounded: false,
            fall_speed: 0.0,
            tags: Tags::JUMP_ENVELOPE,
            trace: None,
        }
    }
}

fn launch(facts: &Facts) -> f64 {
    JUMP_VELOCITY + JUMP_BOOST_STEP * f64::from(facts.jump_boost)
}

impl Check for AirBound {
    fn trace(&self) -> Option<Trace> {
        self.trace
    }

    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        self.trace = None;
        let Serverbound::Move(movement) = packet else {
            return None;
        };
        let position = movement.position?;
        let terrain = facts.terrain.unwrap_or_default();
        let grounded = terrain.supported && !terrain.soft;
        if facts.teleport_pending
            || !terrain.known
            || terrain.soft
            || terrain.supported
            || exempt(facts)
            || facts.velocity
            || facts.levitation
            || facts.slow_falling
        {
            self.bound = launch(facts);
            self.grounded = grounded;
            self.tags = Tags::JUMP_ENVELOPE.with(Tags::JUMP_BOOST, facts.jump_boost > 0);
            if grounded && terrain.bouncy && self.fall_speed + BOUNCE_SLACK > self.bound {
                self.bound = self.fall_speed + BOUNCE_SLACK;
                self.tags = self.tags.with(Tags::BOUNCE, true);
            }
            self.fall_speed = 0.0;
            return None;
        }
        if let Some(launch) = facts.push.launch {
            self.bound = self.bound.max(launch);
            self.tags = self.tags.with(Tags::PUSH, true);
        }
        let dy = position[1] - facts.position[1];
        self.fall_speed = self.fall_speed.max(-dy);
        if replayed(facts) {
            self.bound = ((dy - GRAVITY) * DRAG).max(-3.92);
            self.grounded = false;
            self.tags = Tags::JUMP_ENVELOPE.with(Tags::PREDICTED, true);
            return None;
        }
        let step = self.grounded && STEP_HEIGHT > self.bound;
        let reach = if step { STEP_HEIGHT } else { self.bound };
        let allowed = reach + VERTICAL_SLACK;
        self.trace = Trace::new("fly", allowed, dy, self.tags.with(Tags::STEP_UP, step));
        let velocity = if dy > self.bound + VERTICAL_SLACK {
            0.0
        } else if dy > 0.0 {
            self.bound.min(dy)
        } else {
            self.bound
        };
        if velocity < self.bound - 1.0e-9 {
            self.tags = self.tags.with(Tags::REBASED, true);
        }
        self.bound = ((velocity - GRAVITY) * DRAG).max(-3.92);
        self.grounded = false;
        if dy > allowed {
            setback(facts, "fly", 1.0 + ((dy - allowed) * 4.0).min(3.0))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod course;
#[cfg(test)]
mod sim;
#[cfg(test)]
mod sources;
#[cfg(test)]
mod swim;

#[cfg(test)]
#[path = "movement_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "movement_use_tests.rs"]
mod use_tests;

#[cfg(test)]
#[path = "movement_sustained_tests.rs"]
mod sustained_tests;
