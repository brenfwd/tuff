use super::super::Facts;
use super::super::predict::{Keys, sin_cos, table_sin};
use super::super::velocity::Impulse;
use crate::trace::Tags;
use tuff_core::world::block_contact;
use tuff_core::world::contact::{self, Current, Submersion, WATER_CURRENT_SCALE};
use tuff_core::world::fluid_api::FluidBehaviors;
use tuff_core::world::freezing;
use tuff_core::world::physics::{self, Aabb, modified_friction};
use tuff_core::world::session::ShoveKind;
use tuff_core::world::session::mirror::ChunkMirror;
use tuff_vanilla_common::scaffolding;
use tuff_vanilla_data::tag;
use tuff_vanilla_data::{Block, BlockState};
use tuff_world_model::BlockPos;

/// How far a Riptide launch lifts a player standing on the ground.
pub(super) const RIPTIDE_HOP: f64 = 1.199_999_928_474_426_3;
/// How close a replayed tick must end to the reported position, per axis.
pub const MATCH: f64 = 1.0e-3;
/// How far a move may go past the farthest replay before it is flagged.
pub const HORIZONTAL_TOLERANCE: f64 = 1.0e-3;
pub const VERTICAL_TOLERANCE: f64 = 5.0e-3;
/// Extra horizontal allowance for a move from a seeded state, whose
/// velocity is only estimated from the move before.
pub(super) const SEEDED_SLACK: f64 = 0.03;
/// Moves after a reset that may re-seed instead of carrying the replay.
pub(super) const SEED_MOVES: u8 = 5;
/// Moves that follow a placement the server has not confirmed yet.
pub(super) const WORLD_AHEAD_MOVES: u8 = 2;
/// What one nearby entity may push the player per tick, per axis.
pub(super) const PUSH_PER_ENTITY: f64 = 0.05;
pub(super) const MAX_PUSHERS: usize = 4;
/// How close (per horizontal axis) an entity must be to count as crowding
/// a move the replay could not explain.
pub(super) const CROWD_REACH: f64 = 2.0;
/// How far off an axis an entity's tracked centre must lie for its push
/// on that axis to have a known sign, given the client interpolates it.
pub(super) const PUSH_SIGN_MARGIN: f64 = 0.25;
/// How much farther than its box an entity may reach the player, for its
/// interpolation on the client and a boat's wider push.
pub(super) const PUSH_MARGIN: f64 = 0.3;
/// Searched unreported ticks; any beyond are replayed without input.
pub(super) const MAX_HIDDEN: u32 = 2;
pub(super) const MAX_IDLE: u32 = 40;
pub(super) const RECORDED: usize = 64;
pub(super) const SILENT_SQ: f64 = 2.0e-4 * 2.0e-4;
pub(super) const HALF_WIDTH: f64 = 0.3f32 as f64;
/// How far from its centre, in widths, the client probes for a
/// suffocating block to push out of, and how fast it pushes.
pub(super) const PUSH_OUT_PROBE: f64 = 0.35;
pub(super) const PUSH_OUT_SPEED: f64 = 0.1;
pub(super) const HEIGHT: f64 = 1.8f32 as f64;
pub(super) const CROUCH_HEIGHT: f64 = 1.5f32 as f64;
pub(super) const PRONE_HEIGHT: f64 = 0.6f32 as f64;
pub(super) const EYE_HEIGHT: f64 = 1.62f32 as f64;
pub(super) const CROUCH_EYE_HEIGHT: f64 = 1.27f32 as f64;
pub(super) const PRONE_EYE_HEIGHT: f64 = 0.4f32 as f64;
/// How fast a swimmer's vertical speed eases toward its look, and the
/// faster rate when it looks this far down.
pub(super) const SWIM_EASE: f64 = 0.06;
pub(super) const DIVE_EASE: f64 = 0.085;
pub(super) const DIVE_LOOK: f64 = -0.2;
pub(super) const DEGREES: f32 = 0.017_453_292;
pub(super) const LEVITATION_STEP: f64 = 0.05;
pub(super) const LEVITATION_EASE: f64 = 0.2;
/// Ticks after the server changed a state the client learns of late
/// (levitation, gliding) in which the client may still move as before.
pub(super) const STATE_CATCH_UP: u64 = 40;
/// Ticks past a round trip in which the client may still see the
/// firework the server already removed.
pub(super) const BOOST_CATCH_UP: u64 = 5;

/// The client's rocket rolls its own lifetime, so it can outlive the server's boost.
pub(super) const BOOST_LIFETIME_SPREAD: u64 = 12;
pub(super) const SPRINT_JUMP: f64 = 0.2;
pub(super) const CLIMB_SPEED: f64 = 0.2;
pub(super) const CLIMB_LIMIT: f64 = 0.15000000596046448;
pub(super) const NO_JUMP_DELAY: u8 = 10;
pub(super) const INPUT_DAMPING: f32 = 0.98;
pub(super) const SPRINT_FACTOR: f64 = 1.3;
pub(super) const SPEED_STEP: f64 = 0.2;
/// The input scale of an item in use; a client the server believes idle
/// may still be using one, never the other way round.
pub(super) const USING_SCALE: f32 = 0.2;
pub(super) const COLLISION_EPSILON: f64 = 1.0e-5;
/// How close a box face must be to a block to have touched it.
pub(super) const CONTACT: f64 = 1.0e-6;
/// The fluid depth under which a player wades rather than swims up.
pub(super) const FLUID_JUMP_THRESHOLD: f64 = 0.4;
/// What holding jump in a fluid, or sneak in water, adds to the rise.
pub(super) const FLUID_STROKE: f64 = 0.039_999_999_105_930_33;
pub(super) const WATER_VERTICAL_DRAG: f64 = 0.800_000_011_920_929;
pub(super) const FLUID_EXIT_LIFT: f64 = 0.300_000_011_920_929;
pub(super) const FLUID_EXIT_CLEARANCE: f64 = 0.600_000_023_841_857_9;
pub(super) const MIN_CURRENT_SQ: f64 = 9.999_999_747_378_752e-6;
pub(super) const SLOW_CURRENT: f64 = 0.003;
pub(super) const MIN_CURRENT_PUSH: f64 = 0.004_500_000_000_000_000_5;
/// The lava current's strength where lava is fast and where it is slow.
pub const LAVA_CURRENT_SCALE: [f64; 2] = [0.007, 0.002_333_333_333_333_333_5];
/// Ticks after the server gave or took Dolphin's Grace in which the client
/// may still swim as before.
pub(super) const DOLPHINS_CATCH_UP: u64 = 40;

/// The client's movement attributes, as last synced to it, without the
/// sprint modifier.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Body {
    pub speed: f64,
    pub jump_strength: f32,
    pub gravity: f64,
    pub step_height: f32,
    pub sneak_speed: f32,
    pub efficiency: f32,
    pub water_efficiency: f32,
    pub friction_modifier: f32,
    pub air_drag_modifier: f32,
    pub bounciness: f64,
    /// The speed came from the client's synced attributes, so it already
    /// includes the Speed and Slowness effects.
    pub synced: bool,
}

impl Default for Body {
    fn default() -> Self {
        Self {
            speed: 0.1,
            jump_strength: 0.42,
            gravity: 0.08,
            step_height: 0.6,
            sneak_speed: 0.3,
            efficiency: 0.0,
            water_efficiency: 0.0,
            friction_modifier: 1.0,
            air_drag_modifier: 1.0,
            bounciness: 0.0,
            synced: false,
        }
    }
}

/// How the engine judged a position move.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Prediction {
    /// Horizontal distance past the farthest replay, beyond tolerance.
    pub horizontal: f64,
    /// Rise past the highest replay, beyond tolerance.
    pub vertical: f64,
    /// The farthest horizontal distance a replay reached.
    pub allowed: f64,
    pub actual: f64,
    pub tags: Tags,
    pub tried: u32,
    /// Server velocity was pending and no replay placed this move.
    pub unplaced: bool,
}

impl Prediction {
    pub fn excessive(&self) -> bool {
        self.horizontal > 0.0 || self.vertical > 0.0
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct State {
    pub(super) position: [f64; 3],
    pub(super) velocity: [f64; 3],
    pub(super) on_ground: bool,
    pub(super) horizontal_collision: bool,
    pub(super) supporting: Option<BlockPos>,
    pub(super) no_jump: u8,
    pub(super) fall: f64,
    pub(super) stuck: Option<[f64; 3]>,
    /// The body was inside powder snow when the last tick ended.
    pub(super) in_snow: bool,
    /// The pose the last tick left, which sets the box.
    pub(super) pose: Pose,
    /// In water, and the eyes under it, as the last fluid update saw.
    pub(super) in_water: bool,
    pub(super) eyes: bool,
    pub(super) swimming: bool,
    /// Sprinting at the end of the last tick, which decides the swim.
    pub(super) sprinting: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Pose {
    #[default]
    Standing,
    Crouching,
    /// Gliding or swimming, both as tall as wide.
    Prone,
}

impl Pose {
    pub(super) fn height(self) -> f64 {
        match self {
            Pose::Standing => HEIGHT,
            Pose::Crouching => CROUCH_HEIGHT,
            Pose::Prone => PRONE_HEIGHT,
        }
    }

    pub(super) fn eye_height(self) -> f64 {
        match self {
            Pose::Standing => EYE_HEIGHT,
            Pose::Crouching => CROUCH_EYE_HEIGHT,
            Pose::Prone => PRONE_EYE_HEIGHT,
        }
    }
}

/// What the client held during one tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Input {
    pub(super) keys: Keys,
    pub(super) jump: bool,
    pub(super) sprint: bool,
    /// Crouching, which scales the input down.
    pub(super) slow: bool,
    /// The shift key, which backs off edges and stops bounces and ladder
    /// slides.
    pub(super) shift: bool,
    pub(super) item: f32,
    pub(super) yaw: f32,
    pub(super) pitch: f32,
}

/// What holds for the whole move.
#[derive(Clone, Copy)]
pub(super) struct Rules {
    pub(super) body: Body,
    pub(super) speed_amplifier: Option<u8>,
    pub(super) jump_boost: f32,
    pub(super) slow_falling: bool,
    pub(super) dolphins_grace: bool,
    pub(super) lava_current: f64,
    /// The Levitation amplifier.
    pub(super) levitation: Option<u8>,
    pub(super) gliding: bool,
    /// Leather boots let it stand on powder snow.
    pub(super) snow_walker: bool,
    /// Spinning from a Riptide launch, in the spin pose.
    pub(super) spinning: bool,
    /// The Weaving effect, which cobwebs hold back less.
    pub(super) weaving: bool,
}

impl Rules {
    pub(super) fn movement_speed(&self, sprint: bool) -> f32 {
        let mut speed = self.body.speed;
        if !self.body.synced
            && let Some(amplifier) = self.speed_amplifier
        {
            speed *= 1.0 + SPEED_STEP * (f64::from(amplifier) + 1.0);
        }
        if sprint {
            speed *= SPRINT_FACTOR;
        }
        speed as f32
    }

    pub(super) fn gravity(&self, vertical: f64) -> f64 {
        if self.slow_falling && vertical <= 0.0 {
            self.body.gravity.min(0.01)
        } else {
            self.body.gravity
        }
    }

    /// The vertical speed after the pull of one tick in the air, before
    /// drag: gravity, or levitation easing toward its rise.
    pub(super) fn pull(&self, vertical: f64) -> f64 {
        match self.levitation {
            Some(amplifier) => {
                vertical
                    + (LEVITATION_STEP * (f64::from(amplifier) + 1.0) - vertical) * LEVITATION_EASE
            }
            None => vertical - self.gravity(vertical),
        }
    }
}

/// What the shape of powder snow and scaffolding depends on.
#[derive(Clone, Copy)]
pub(super) struct Context {
    pub(super) fall: f64,
    pub(super) walker: bool,
    pub(super) descending: bool,
}

impl Context {
    pub(super) fn seeded(rules: &Rules) -> Context {
        Context {
            fall: 0.0,
            walker: rules.snow_walker,
            descending: false,
        }
    }
}

pub(super) fn context_of(s: &State, input: &Input, rules: &Rules) -> Context {
    Context {
        fall: s.fall,
        walker: rules.snow_walker,
        descending: input.shift,
    }
}

pub(super) struct World<'a> {
    pub(super) mirror: &'a ChunkMirror,
    pub(super) fluids: &'a dyn FluidBehaviors,
    /// Boxes of the entities the client collides with (boats, shulkers).
    pub(super) solids: &'a [Aabb],
}

impl World<'_> {
    pub(super) fn block(&self, pos: BlockPos) -> Option<BlockState> {
        self.mirror.block_at(pos)
    }

    /// How deep the body at `position` stands in water and lava, and the
    /// currents it feels there.
    pub(super) fn immersion(&self, position: [f64; 3], pose: Pose) -> Submersion {
        let block = |pos: BlockPos| self.mirror.block_at(pos);
        contact::submersion(body(position, pose), block, |pos| {
            self.fluids.flow(&block, pos)
        })
    }

    /// Any block in `area` holds a fluid.
    pub(super) fn any_liquid(&self, area: Aabb) -> bool {
        let range = |lo: f64, hi: f64| lo.floor() as i32..hi.ceil() as i32;
        range(area[0], area[3]).any(|x| {
            range(area[1], area[4]).any(|y| {
                range(area[2], area[5]).any(|z| {
                    self.block(BlockPos::new(x, y, z))
                        .is_some_and(|state| !state.fluid_state().is_empty())
                })
            })
        })
    }

    pub(super) fn water_at(&self, pos: BlockPos) -> bool {
        self.block(pos).is_some_and(|state| {
            tuff_datapack::tags::is(tag::fluid::key::WATER, state.fluid_state().fluid())
        })
    }

    /// The eyes of the body at `position` are under water, as the camera
    /// sees the surface.
    pub(super) fn eyes_in_water(&self, position: [f64; 3], pose: Pose) -> bool {
        let eye = [position[0], position[1] + pose.eye_height(), position[2]];
        contact::eyes_in(eye, tag::fluid::key::WATER, |pos| self.block(pos))
    }

    /// The body fits at `position` in `pose`, with the shapes `context` gives
    /// scaffolding and powder snow.
    pub(super) fn fits(&self, position: [f64; 3], pose: Pose, context: Context) -> bool {
        let area = physics::inflate(body(position, pose), [-1.0e-7; 3]);
        physics::colliders_with(
            area,
            &|pos| self.shape(pos, position[1], context),
            self.solids,
        )
        .is_empty()
    }

    pub(super) fn free_in(&self, area: Aabb, bottom: f64, context: Context) -> bool {
        physics::colliders_with(area, &|pos| self.shape(pos, bottom, context), self.solids)
            .is_empty()
    }

    pub(super) fn free(&self, area: Aabb) -> bool {
        self.mirror.colliders(area).is_empty()
            && !self.solids.iter().any(|b| physics::intersects(*b, area))
    }

    pub(super) fn shape(&self, pos: BlockPos, bottom: f64, context: Context) -> physics::Shape {
        let state = self.block(pos);
        match state {
            Some(s) if s.block() == Block::PowderSnow => {
                freezing::powder_snow_boxes(pos, bottom, context.fall, context.walker).into()
            }
            Some(s) if s.block() == Block::Scaffolding => {
                scaffolding::collision_boxes(s, pos, bottom, context.descending).into()
            }
            _ => physics::Shape::collision(state, pos),
        }
    }

    pub(super) fn collide(
        &self,
        bounds: Aabb,
        delta: [f64; 3],
        step: f32,
        on_ground: bool,
        context: Context,
    ) -> [f64; 3] {
        physics::collide_with(
            bounds,
            delta,
            step,
            on_ground,
            &|pos| self.shape(pos, bounds[1], context),
            self.solids,
        )
    }

    pub(super) fn movement_at(&self, pos: BlockPos) -> tuff_vanilla_data::entity::BlockMovement {
        self.block(pos)
            .unwrap_or(Block::Air.default_state())
            .block()
            .movement()
    }

    pub(super) fn on_pos(&self, state: &State, offset: f32) -> BlockPos {
        block_contact::on_pos(state.position, state.supporting, offset, &|pos| {
            self.block(pos)
        })
    }

    pub(super) fn climbable(&self, position: [f64; 3]) -> bool {
        let pos = physics::block_pos(position);
        let Some(state) = self.block(pos) else {
            return false;
        };
        if tuff_datapack::tags::is(tag::block::key::CLIMBABLE, state.block()) {
            return true;
        }
        tuff_datapack::tags::is(tag::block::key::TRAPDOORS, state.block())
            && state.get("open") == Some("true")
            && self
                .block(BlockPos::new(pos.x, pos.y - 1, pos.z))
                .is_some_and(|below| {
                    below.block() == Block::Ladder && below.get("facing") == state.get("facing")
                })
    }

    pub(super) fn speed_factor(&self, state: &State, efficiency: f32) -> f32 {
        let here = physics::block_pos(state.position);
        let water = self
            .block(here)
            .is_some_and(|s| matches!(s.block(), Block::Water | Block::BubbleColumn));
        let factor = self.movement_at(here).speed_factor;
        let base = if water || factor != 1.0 {
            factor
        } else {
            self.movement_at(self.on_pos(state, 0.500_001)).speed_factor
        };
        base + (1.0 - base) * efficiency
    }

    pub(super) fn jump_factor(&self, state: &State) -> f32 {
        let here = self
            .movement_at(physics::block_pos(state.position))
            .jump_factor;
        if here == 1.0 {
            self.movement_at(self.on_pos(state, 0.500_001)).jump_factor
        } else {
            here
        }
    }

    /// No collider lies in the column `min_height` under the feet shifted
    /// by `(dx, dz)`.
    pub(super) fn can_fall(
        &self,
        position: [f64; 3],
        dx: f64,
        dz: f64,
        min_height: f64,
        context: Context,
    ) -> bool {
        let b = bounds(position);
        self.free_in(
            [
                b[0] + 1.0e-7 + dx,
                b[1] - min_height - 1.0e-7,
                b[2] + 1.0e-7 + dz,
                b[3] - 1.0e-7 + dx,
                b[1],
                b[5] - 1.0e-7 + dz,
            ],
            b[1],
            context,
        )
    }

    /// A suffocating block's collision fills part of column `pos` over the
    /// height of `body`.
    pub(super) fn suffocates_at(&self, pos: BlockPos, body: Aabb) -> bool {
        let area = physics::inflate(
            [
                f64::from(pos.x),
                body[1],
                f64::from(pos.z),
                f64::from(pos.x) + 1.0,
                body[4],
                f64::from(pos.z) + 1.0,
            ],
            [-1.0e-7; 3],
        );
        let lowest = area[1].floor() as i32;
        let highest = area[4].floor() as i32;
        (lowest..=highest).any(|y| {
            let cell = BlockPos::new(pos.x, y, pos.z);
            self.block(cell).is_some_and(|state| {
                state.is_suffocating()
                    && physics::block_boxes(Some(state), cell).any(|b| physics::intersects(b, area))
            })
        })
    }

    /// A moving piston, which the replay does not model, touches the body
    /// anywhere from `from` to `to`.
    pub(super) fn unmodeled(&self, from: [f64; 3], to: [f64; 3]) -> bool {
        let a = bounds(from);
        let b = bounds(to);
        let area = [
            a[0].min(b[0]) - 0.1,
            a[1].min(b[1]) - 0.1,
            a[2].min(b[2]) - 0.1,
            a[3].max(b[3]) + 0.1,
            a[4].max(b[4]) + 0.1,
            a[5].max(b[5]) + 0.1,
        ];
        let floor = |v: f64| v.floor() as i32;
        (floor(area[0])..=floor(area[3])).any(|x| {
            (floor(area[1])..=floor(area[4])).any(|y| {
                (floor(area[2])..=floor(area[5])).any(|z| {
                    self.block(BlockPos::new(x, y, z))
                        .is_some_and(|state| state.block() == Block::MovingPiston)
                })
            })
        })
    }

    /// The blocks the body touches while it moves from `from` to `to`, as
    /// cells, with the box at the end of the move.
    pub(super) fn touched(
        &self,
        from: [f64; 3],
        to: [f64; 3],
        pose: Pose,
    ) -> (Vec<BlockPos>, Aabb) {
        let a = body(from, pose);
        let b = body(to, pose);
        let area = [
            a[0].min(b[0]) + COLLISION_EPSILON,
            a[1].min(b[1]) + COLLISION_EPSILON,
            a[2].min(b[2]) + COLLISION_EPSILON,
            a[3].max(b[3]) - COLLISION_EPSILON,
            a[4].max(b[4]) - COLLISION_EPSILON,
            a[5].max(b[5]) - COLLISION_EPSILON,
        ];
        let floor = |v: f64| v.floor() as i32;
        let mut cells = Vec::new();
        for x in floor(area[0])..=floor(area[3]) {
            for y in floor(area[1])..=floor(area[4]) {
                for z in floor(area[2])..=floor(area[5]) {
                    cells.push(BlockPos::new(x, y, z));
                }
            }
        }
        let end = [
            b[0] + COLLISION_EPSILON,
            b[1] + COLLISION_EPSILON,
            b[2] + COLLISION_EPSILON,
            b[3] - COLLISION_EPSILON,
            b[4] - COLLISION_EPSILON,
            b[5] - COLLISION_EPSILON,
        ];
        (cells, end)
    }

    /// The slowdown the blocks swept through set for the next move.
    pub(super) fn stuck(
        &self,
        from: [f64; 3],
        to: [f64; 3],
        pose: Pose,
        weaving: bool,
    ) -> Option<[f64; 3]> {
        block_contact::swept_contact(
            tuff_vanilla_data::registry::EntityType::Player,
            body(from, pose),
            body(to, pose),
            None,
            &|pos| self.block(pos),
        )
        .stuck
        .map(|stuck| stuck.multiplier(weaving))
    }

    pub(super) fn in_powder_snow(&self, from: [f64; 3], to: [f64; 3], pose: Pose) -> bool {
        self.touched(from, to, pose).0.into_iter().any(|pos| {
            self.block(pos)
                .is_some_and(|s| s.block() == Block::PowderSnow)
        })
    }

    /// What the blocks swept through do to the velocity a tick ends with:
    /// bubble columns lift or drag, honey holds a body sliding down its
    /// side; powder snow is noted for the next tick's climb.
    pub(super) fn effects(&self, s: &mut State, from: [f64; 3], marks: &mut Marks) {
        let (cells, end) = self.touched(from, s.position, s.pose);
        let far = (0..3)
            .map(|a| (s.position[a] - from[a]).powi(2))
            .sum::<f64>()
            > 0.999_99 * 0.999_99;
        s.in_snow = false;
        for pos in cells {
            let Some(state) = self.block(pos) else {
                continue;
            };
            match state.block() {
                Block::BubbleColumn => {
                    let inside = far
                        || (end[0] < f64::from(pos.x + 1)
                            && end[3] > f64::from(pos.x)
                            && end[1] < f64::from(pos.y + 1)
                            && end[4] > f64::from(pos.y)
                            && end[2] < f64::from(pos.z + 1)
                            && end[5] > f64::from(pos.z));
                    if !inside {
                        continue;
                    }
                    let drag = state.get("drag") == Some("true");
                    let open = self
                        .block(BlockPos::new(pos.x, pos.y + 1, pos.z))
                        .is_none_or(|above| {
                            above.collision_boxes().is_empty() && above.fluid_state().is_empty()
                        });
                    marks.fluid = true;
                    let v = &mut s.velocity[1];
                    *v = match (open, drag) {
                        (true, true) => (*v - 0.03).max(-0.9),
                        (true, false) => (*v + 0.1).min(1.8),
                        (false, true) => (*v - 0.03).max(-0.3),
                        (false, false) => (*v + 0.06).min(0.7),
                    };
                    if !open {
                        s.fall = 0.0;
                    }
                }
                Block::HoneyBlock => {
                    let old = s.velocity[1] / 0.980_000_019_073_486_3 + 0.08;
                    let overlap = 0.4375 + HALF_WIDTH;
                    let dx = (f64::from(pos.x) + 0.5 - s.position[0]).abs();
                    let dz = (f64::from(pos.z) + 0.5 - s.position[2]).abs();
                    if !s.on_ground
                        && s.position[1] <= f64::from(pos.y) + 0.9375 - 1.0e-7
                        && old < -0.08
                        && (dx + 1.0e-7 > overlap || dz + 1.0e-7 > overlap)
                    {
                        let slid = (-0.05 - 0.08) * 0.980_000_019_073_486_3;
                        if old < -0.13 {
                            let reduce = -0.05 / old;
                            s.velocity[0] *= reduce;
                            s.velocity[2] *= reduce;
                        }
                        s.velocity[1] = slid;
                        s.fall = 0.0;
                    }
                }
                Block::PowderSnow => s.in_snow = true,
                _ => {}
            }
        }
    }
}

pub(super) fn bounds(position: [f64; 3]) -> Aabb {
    body(position, Pose::Standing)
}

pub(super) fn body(position: [f64; 3], pose: Pose) -> Aabb {
    let height = pose.height();
    [
        position[0] - HALF_WIDTH,
        position[1],
        position[2] - HALF_WIDTH,
        position[0] + HALF_WIDTH,
        position[1] + height,
        position[2] + HALF_WIDTH,
    ]
}

pub(super) fn length_sq(v: [f64; 3]) -> f64 {
    v[0] * v[0] + v[1] * v[1] + v[2] * v[2]
}

/// The key impulse scaled the way the client scales it: item use and
/// crouching shrink it, then it is stretched toward the square.
pub(super) fn movement_input(keys: Keys, item: f32, sneak: f32) -> [f32; 2] {
    let axis = |pos: u8, neg: u8| {
        f32::from(u8::from(keys.0 & pos != 0)) - f32::from(u8::from(keys.0 & neg != 0))
    };
    let raw = [
        axis(Keys::LEFT, Keys::RIGHT),
        axis(Keys::FORWARD, Keys::BACKWARD),
    ];
    let len = (raw[0] * raw[0] + raw[1] * raw[1]).sqrt();
    if len < 1.0e-4 {
        return [0.0, 0.0];
    }
    let scale = INPUT_DAMPING * item * sneak;
    let v = [raw[0] / len * scale, raw[1] / len * scale];
    let len = (v[0] * v[0] + v[1] * v[1]).sqrt();
    if len <= 0.0 {
        return v;
    }
    let unit = [v[0] / len, v[1] / len];
    let (x, y) = (unit[0].abs(), unit[1].abs());
    let tan = if y > x { x / y } else { y / x };
    let stretched = (len * (1.0 + tan * tan).sqrt()).min(1.0);
    [unit[0] * stretched, unit[1] * stretched]
}

pub(super) fn move_relative(velocity: &mut [f64; 3], input: [f32; 2], speed: f32, yaw: f32) {
    let (mut x, mut z) = (f64::from(input[0]), f64::from(input[1]));
    let len = x * x + z * z;
    if len < 1.0e-7 {
        return;
    }
    if len > 1.0 {
        let len = len.sqrt();
        x /= len;
        z /= len;
    }
    let speed = f64::from(speed);
    x *= speed;
    z *= speed;
    let (sin, cos) = sin_cos(yaw);
    velocity[0] += x * cos - z * sin;
    velocity[2] += z * cos + x * sin;
}

/// The client's sneaking back-off: a move that would leave the block it
/// stands on is shortened in 0.05 steps per axis.
pub(super) fn back_off(
    state: &State,
    delta: [f64; 3],
    step: f64,
    world: &World,
    context: Context,
) -> ([f64; 3], bool) {
    let above_ground = state.on_ground
        || (state.fall < step
            && !world.can_fall(state.position, 0.0, 0.0, step - state.fall, context));
    if delta[1] > 0.0 || !above_ground {
        return (delta, false);
    }
    let p = state.position;
    let (mut dx, mut dz) = (delta[0], delta[2]);
    let step_x = dx.signum() * 0.05;
    let step_z = dz.signum() * 0.05;
    while dx != 0.0 && world.can_fall(p, dx, 0.0, step, context) {
        if dx.abs() <= 0.05 {
            dx = 0.0;
            break;
        }
        dx -= step_x;
    }
    while dz != 0.0 && world.can_fall(p, 0.0, dz, step, context) {
        if dz.abs() <= 0.05 {
            dz = 0.0;
            break;
        }
        dz -= step_z;
    }
    while dx != 0.0 && dz != 0.0 && world.can_fall(p, dx, dz, step, context) {
        dx = if dx.abs() <= 0.05 { 0.0 } else { dx - step_x };
        dz = if dz.abs() <= 0.05 { 0.0 } else { dz - step_z };
    }
    let changed = dx != delta[0] || dz != delta[2];
    ([dx, delta[1], dz], changed)
}

/// What a replayed tick did beyond moving.
#[derive(Clone, Copy, Default)]
pub(super) struct Marks {
    pub(super) jumped: bool,
    pub(super) edge: bool,
    pub(super) step: bool,
    pub(super) bounce: bool,
    pub(super) climb: bool,
    pub(super) stuck: bool,
    pub(super) fluid: bool,
    pub(super) glide: bool,
    pub(super) swim: bool,
    /// The replay assumed a jump delay the state did not show.
    pub(super) jump_delay: bool,
    /// The client pushed itself out of a suffocating block.
    pub(super) pushed_out: bool,
}

/// What pushes a player in a fluid: the mean of the currents it touches,
/// raised to a minimum when it barely moves.
pub(super) fn push_current(velocity: &mut [f64; 3], current: &Current, scale: f64) {
    if current.count == 0 || length_sq(current.sum) < MIN_CURRENT_SQ {
        return;
    }
    let mean = 1.0 / f64::from(current.count);
    let mut impulse = current.sum.map(|c| c * mean * scale);
    let length = length_sq(impulse).sqrt();
    if velocity[0].abs() < SLOW_CURRENT
        && velocity[2].abs() < SLOW_CURRENT
        && length < MIN_CURRENT_PUSH
    {
        impulse = if length < 1.0e-5 {
            [0.0; 3]
        } else {
            impulse.map(|c| c / length * MIN_CURRENT_PUSH)
        };
    }
    for axis in 0..3 {
        velocity[axis] += impulse[axis];
    }
}

/// The client nudges a body caught in a suffocating block toward the
/// nearest free side, probing four points around its centre.
pub(super) fn push_out(s: &mut State, world: &World, marks: &mut Marks) {
    let reach = HALF_WIDTH * 2.0 * PUSH_OUT_PROBE;
    let body = body(s.position, s.pose);
    for (dx, dz) in [(-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0), (1.0, 1.0)] {
        let x = s.position[0] + dx * reach;
        let z = s.position[2] + dz * reach;
        let pos = physics::block_pos([x, s.position[1], z]);
        if !world.suffocates_at(pos, body) {
            continue;
        }
        let (xd, zd) = (x - f64::from(pos.x), z - f64::from(pos.z));
        let mut best: Option<(usize, f64)> = None;
        let mut closest = f64::MAX;
        for (axis, step, edge) in [(0, -1, xd), (0, 1, 1.0 - xd), (2, -1, zd), (2, 1, 1.0 - zd)] {
            let side = if axis == 0 {
                BlockPos::new(pos.x + step, pos.y, pos.z)
            } else {
                BlockPos::new(pos.x, pos.y, pos.z + step)
            };
            if edge < closest && !world.suffocates_at(side, body) {
                closest = edge;
                best = Some((axis, f64::from(step)));
            }
        }
        if let Some((axis, step)) = best {
            s.velocity[axis] = PUSH_OUT_SPEED * step;
            marks.pushed_out = true;
        }
    }
}

/// One client tick of a walking or wading player.
pub(super) fn tick(
    mut s: State,
    input: &Input,
    rules: &Rules,
    world: &World,
    marks: &mut Marks,
) -> State {
    let from = s.position;
    let fluid = world.immersion(from, s.pose);
    let in_water = fluid.water > 0.0;
    let in_lava = fluid.lava > 0.0;
    marks.fluid |= in_water || in_lava;
    let eyes_were_in = s.eyes;
    s.in_water = in_water;
    s.eyes = in_water && world.eyes_in_water(from, s.pose);
    s.swimming = s.sprinting
        && in_water
        && (s.swimming || (eyes_were_in && world.water_at(physics::block_pos(from))));
    marks.swim |= s.swimming;
    let input = &Input {
        slow: moving_slowly(&s, input, rules, world),
        ..*input
    };
    if in_water {
        s.fall = 0.0;
        push_current(&mut s.velocity, &fluid.water_current, WATER_CURRENT_SCALE);
    }
    if in_lava {
        s.fall *= 0.5;
        push_current(&mut s.velocity, &fluid.lava_current, rules.lava_current);
    }
    push_out(&mut s, world, marks);
    if in_water && input.shift {
        s.velocity[1] -= FLUID_STROKE;
    }
    s.no_jump = s.no_jump.saturating_sub(1);
    {
        let v = &mut s.velocity;
        if v[0] * v[0] + v[2] * v[2] < 9.0e-6 {
            v[0] = 0.0;
            v[2] = 0.0;
        }
        if v[1].abs() < 0.003 {
            v[1] = 0.0;
        }
    }
    if input.jump {
        let depth = if in_lava { fluid.lava } else { fluid.water };
        let wading = in_water && depth > 0.0;
        let stroke = (wading && (!s.on_ground || depth > FLUID_JUMP_THRESHOLD))
            || (in_lava && (!s.on_ground || fluid.lava > FLUID_JUMP_THRESHOLD));
        if stroke {
            s.velocity[1] += FLUID_STROKE;
        } else if (s.on_ground || (wading && depth <= FLUID_JUMP_THRESHOLD)) && s.no_jump == 0 {
            let power = rules.body.jump_strength * world.jump_factor(&s) + rules.jump_boost;
            if power > 1.0e-5 {
                marks.jumped = true;
                s.velocity[1] = s.velocity[1].max(f64::from(power));
                if input.sprint {
                    let (sin, cos) = sin_cos(input.yaw);
                    s.velocity[0] -= sin * SPRINT_JUMP;
                    s.velocity[2] += cos * SPRINT_JUMP;
                }
            }
            s.no_jump = NO_JUMP_DELAY;
        }
    } else {
        s.no_jump = 0;
    }
    if rules.slow_falling || rules.levitation.is_some() {
        s.fall = 0.0;
    }
    if s.swimming {
        swim_toward_look(&mut s, input, world);
    }
    if in_water || in_lava {
        s = wade(s, input, rules, world, marks, &fluid);
    } else if rules.gliding && !world.climbable(s.position) {
        marks.glide = true;
        s.velocity = fall_flying(
            s.velocity,
            input.yaw,
            input.pitch,
            rules.gravity(s.velocity[1]),
        );
        shift(&mut s, input, rules, world, marks);
    } else {
        s = walk(s, input, rules, world, marks);
    }
    if s.on_ground
        && !input.shift
        && world
            .block(world.on_pos(&s, 0.2))
            .is_some_and(|b| b.block() == Block::SlimeBlock)
    {
        let vy = s.velocity[1].abs();
        if vy < 0.1 {
            let scale = 0.4 + vy * 0.2;
            s.velocity[0] *= scale;
            s.velocity[2] *= scale;
        }
    }
    world.effects(&mut s, from, marks);
    s.stuck = world.stuck(from, s.position, s.pose, rules.weaving);
    s.pose = pose_after(&s, input, rules, world);
    s.sprinting = input.sprint;
    s
}

/// The client scales its input down while crouching (shift held, or no
/// room to stand) and while crawling out of water.
pub(super) fn moving_slowly(s: &State, input: &Input, rules: &Rules, world: &World) -> bool {
    if s.pose == Pose::Prone && !rules.gliding && !rules.spinning && !s.in_water {
        return true;
    }
    if s.swimming {
        return false;
    }
    let context = context_of(s, input, rules);
    match s.pose {
        Pose::Standing => false,
        Pose::Crouching => world.fits(s.position, Pose::Crouching, context),
        _ => {
            (input.slow || !world.fits(s.position, Pose::Standing, context))
                && world.fits(s.position, Pose::Crouching, context)
        }
    }
}

/// A swimmer's vertical speed eases toward where it looks, unless it
/// looks up with its head out of the water and does not hold jump.
pub(super) fn swim_toward_look(s: &mut State, input: &Input, world: &World) {
    let rise = view(input.yaw, input.pitch)[1];
    let ease = if rise < DIVE_LOOK {
        DIVE_EASE
    } else {
        SWIM_EASE
    };
    let p = s.position;
    let head = physics::block_pos([p[0], p[1] + 1.0 - 0.1, p[2]]);
    let submerged = world
        .block(head)
        .is_some_and(|state| !state.fluid_state().is_empty());
    if rise <= 0.0 || input.jump || submerged {
        s.velocity[1] += (rise - s.velocity[1]) * ease;
    }
}

/// The pose the client takes at the end of a tick.
pub(super) fn pose_after(s: &State, input: &Input, rules: &Rules, world: &World) -> Pose {
    let fits = |pose: Pose| world.fits(s.position, pose, context_of(s, input, rules));
    if !fits(Pose::Prone) {
        return s.pose;
    }
    let desired = if s.swimming || rules.gliding || rules.spinning {
        Pose::Prone
    } else if input.shift {
        Pose::Crouching
    } else {
        Pose::Standing
    };
    if fits(desired) {
        desired
    } else if fits(Pose::Crouching) {
        Pose::Crouching
    } else {
        Pose::Prone
    }
}

/// The direction a look points, with the game's sine table.
pub(super) fn view(yaw: f32, pitch: f32) -> [f64; 3] {
    let x = pitch * DEGREES;
    let y = -yaw * DEGREES;
    let sin = |v: f32| table_sin(v * 10430.378) as f32;
    let cos = |v: f32| table_sin(v * 10430.378 + 16384.0) as f32;
    let (y_cos, y_sin, x_cos, x_sin) = (cos(y), sin(y), cos(x), sin(x));
    [
        f64::from(y_sin * x_cos),
        f64::from(-x_sin),
        f64::from(y_cos * x_cos),
    ]
}

/// One tick of elytra flight's lift, dive and turn toward the look.
pub(super) fn fall_flying(v: [f64; 3], yaw: f32, pitch: f32, gravity: f64) -> [f64; 3] {
    let look = view(yaw, pitch);
    let lean = pitch * DEGREES;
    let look_horizontal = (look[0] * look[0] + look[2] * look[2]).sqrt();
    let move_horizontal = (v[0] * v[0] + v[2] * v[2]).sqrt();
    let cos = f64::from(lean).cos();
    let lift = cos * cos;
    let mut m = [v[0], v[1] + gravity * (-1.0 + lift * 0.75), v[2]];
    if m[1] < 0.0 && look_horizontal > 0.0 {
        let convert = m[1] * -0.1 * lift;
        m[0] += look[0] * convert / look_horizontal;
        m[1] += convert;
        m[2] += look[2] * convert / look_horizontal;
    }
    if lean < 0.0 && look_horizontal > 0.0 {
        let sin = table_sin(lean * 10430.378) as f32;
        let convert = move_horizontal * f64::from(-sin) * 0.04;
        m[0] += -look[0] * convert / look_horizontal;
        m[1] += convert * 3.2;
        m[2] += -look[2] * convert / look_horizontal;
    }
    if look_horizontal > 0.0 {
        m[0] += (look[0] / look_horizontal * move_horizontal - m[0]) * 0.1;
        m[2] += (look[2] / look_horizontal * move_horizontal - m[2]) * 0.1;
    }
    [
        m[0] * 0.990_000_009_536_743_2,
        m[1] * 0.980_000_019_073_486_3,
        m[2] * 0.990_000_009_536_743_2,
    ]
}

/// A firework rocket's pull on the glider it is attached to.
pub(super) fn firework_boost(v: [f64; 3], look: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|a| v[a] + (look[a] * 0.1 + (look[a] * 1.5 - v[a]) * 0.5))
}

pub(super) fn input_push(s: &mut State, input: &Input, rules: &Rules, speed: f32) {
    let sneak = if input.slow {
        rules.body.sneak_speed
    } else {
        1.0
    };
    move_relative(
        &mut s.velocity,
        movement_input(input.keys, input.item, sneak),
        speed,
        input.yaw,
    );
}

/// The movement part of a tick out of fluids: acceleration from the
/// ground or air, climbing, then gravity and drag.
pub(super) fn walk(
    mut s: State,
    input: &Input,
    rules: &Rules,
    world: &World,
    marks: &mut Marks,
) -> State {
    let block_friction = if s.on_ground {
        modified_friction(
            world.movement_at(world.on_pos(&s, 0.500_001)).friction,
            rules.body.friction_modifier,
        )
    } else {
        1.0
    };
    let speed = if !s.on_ground {
        if input.sprint { 0.025_999_999 } else { 0.02 }
    } else {
        let base = rules.movement_speed(input.sprint);
        if f64::from(block_friction) > 0.6 {
            base * (0.216_000_02 / (block_friction * block_friction * block_friction))
        } else {
            base
        }
    };
    input_push(&mut s, input, rules, speed);
    if world.climbable(s.position) {
        marks.climb = true;
        s.fall = 0.0;
        let v = &mut s.velocity;
        v[0] = v[0].clamp(-CLIMB_LIMIT, CLIMB_LIMIT);
        v[2] = v[2].clamp(-CLIMB_LIMIT, CLIMB_LIMIT);
        v[1] = v[1].max(-CLIMB_LIMIT);
        let scaffold = world
            .block(physics::block_pos(s.position))
            .is_some_and(|b| b.block() == Block::Scaffolding);
        if v[1] < 0.0 && !scaffold && input.shift {
            v[1] = 0.0;
        }
    }
    shift(&mut s, input, rules, world, marks);
    let mut movement = s.velocity;
    if (s.horizontal_collision || input.jump)
        && (world.climbable(s.position) || (s.in_snow && rules.snow_walker))
    {
        movement[1] = CLIMB_SPEED;
    }
    let y = rules.pull(movement[1]);
    let air = modified_friction(0.91, rules.body.air_drag_modifier);
    let friction = f64::from(block_friction * air);
    let vertical_drag = f64::from(modified_friction(0.98, rules.body.air_drag_modifier));
    s.velocity = [
        movement[0] * friction,
        y * vertical_drag,
        movement[2] * friction,
    ];
    s
}

/// The movement part of a tick in water or lava: weak acceleration, the
/// fluid's drag and sinking, and the lift out over a ledge.
pub(super) fn wade(
    mut s: State,
    input: &Input,
    rules: &Rules,
    world: &World,
    marks: &mut Marks,
    fluid: &Submersion,
) -> State {
    let falling = s.velocity[1] <= 0.0;
    let old_y = s.position[1];
    let gravity = rules.gravity(s.velocity[1]);
    if fluid.water > 0.0 {
        let (slow, speed) = water_drag(rules, s.on_ground, input.sprint);
        input_push(&mut s, input, rules, speed);
        shift(&mut s, input, rules, world, marks);
        let mut movement = s.velocity;
        if s.horizontal_collision && world.climbable(s.position) {
            movement[1] = CLIMB_SPEED;
        }
        let slow = f64::from(slow);
        s.velocity = sink(
            gravity,
            falling,
            input.sprint,
            [
                movement[0] * slow,
                movement[1] * WATER_VERTICAL_DRAG,
                movement[2] * slow,
            ],
        );
    } else {
        input_push(&mut s, input, rules, 0.02);
        shift(&mut s, input, rules, world, marks);
        let v = s.velocity;
        s.velocity = if fluid.lava <= FLUID_JUMP_THRESHOLD {
            sink(
                gravity,
                falling,
                input.sprint,
                [v[0] * 0.5, v[1] * WATER_VERTICAL_DRAG, v[2] * 0.5],
            )
        } else {
            v.map(|c| c * 0.5)
        };
        if gravity != 0.0 {
            s.velocity[1] += -gravity / 4.0;
        }
    }
    let v = s.velocity;
    if s.horizontal_collision {
        let rise = ((v[1] + FLUID_EXIT_CLEARANCE) - s.position[1]) + old_y;
        let b = body(s.position, s.pose);
        let lifted = [
            b[0] + v[0],
            b[1] + rise,
            b[2] + v[2],
            b[3] + v[0],
            b[4] + rise,
            b[5] + v[2],
        ];
        if world.free(lifted) && !world.any_liquid(lifted) {
            s.velocity[1] = FLUID_EXIT_LIFT;
        }
    }
    s
}

/// How much of its velocity a body keeps in water each tick, and how
/// strongly its input accelerates it there.
pub(super) fn water_drag(rules: &Rules, on_ground: bool, sprint: bool) -> (f32, f32) {
    let mut slow: f32 = if sprint { 0.9 } else { 0.8 };
    let mut speed: f32 = 0.02;
    let mut efficiency = rules.body.water_efficiency;
    if !on_ground {
        efficiency *= 0.5;
    }
    if efficiency > 0.0 {
        slow += (0.546_000_06 - slow) * efficiency;
        speed = 0.02 + (rules.movement_speed(sprint) - 0.02) * efficiency;
    }
    if rules.dolphins_grace {
        slow = 0.96;
    }
    (slow, speed)
}

/// A fluid's pull down on a body that does not sprint: a fixed slow sink
/// once it has settled, otherwise a sixteenth of gravity.
pub(super) fn sink(gravity: f64, falling: bool, sprint: bool, v: [f64; 3]) -> [f64; 3] {
    if gravity == 0.0 || sprint {
        return v;
    }
    let y = if falling && (v[1] - 0.005).abs() >= 0.003 && (v[1] - gravity / 16.0).abs() < 0.003 {
        -0.003
    } else {
        v[1] - gravity / 16.0
    };
    [v[0], y, v[2]]
}

/// Moves the body by its velocity against the client's blocks: the
/// cobweb slowdown, edge back-off, collision and step-up, landing and
/// bounces, then the block's speed factor.
pub(super) fn shift(s: &mut State, input: &Input, rules: &Rules, world: &World, marks: &mut Marks) {
    let mut delta = s.velocity;
    if let Some(scale) = s.stuck.take() {
        marks.stuck = true;
        delta = [0, 1, 2].map(|axis| delta[axis] * scale[axis]);
        s.velocity = [0.0; 3];
    }
    if input.shift {
        let (backed, changed) = back_off(
            s,
            delta,
            f64::from(rules.body.step_height),
            world,
            context_of(s, input, rules),
        );
        delta = backed;
        marks.edge |= changed;
    }
    let from = s.position;
    let moved = world.collide(
        body(from, s.pose),
        delta,
        rules.body.step_height,
        s.on_ground,
        context_of(s, input, rules),
    );
    marks.step |= moved[1] > 0.0 && delta[1] <= 0.0;
    let moved_sq = length_sq(moved);
    if moved_sq > 1.0e-7 || length_sq(delta) - moved_sq < 1.0e-7 {
        for axis in 0..3 {
            s.position[axis] += moved[axis];
        }
    }
    let x_hit = (delta[0] - moved[0]).abs() >= COLLISION_EPSILON;
    let z_hit = (delta[2] - moved[2]).abs() >= COLLISION_EPSILON;
    s.horizontal_collision = x_hit || z_hit;
    let vertical = delta[1] != moved[1];
    let below = vertical && delta[1] < 0.0;
    s.on_ground = below;
    s.supporting = if below {
        let here = body(s.position, s.pose);
        let lookup = |pos| world.block(pos);
        physics::supporting_block(here, &lookup).or_else(|| {
            physics::supporting_block(physics::offset(here, [-moved[0], 0.0, -moved[2]]), &lookup)
        })
    } else {
        None
    };
    if !s.in_water {
        refresh_fluids(s, rules, world, marks);
    }
    if s.on_ground {
        s.fall = 0.0;
    } else if moved[1] < 0.0 && !s.in_water {
        s.fall -= moved[1];
    }
    if vertical || x_hit || z_hit {
        let current = s.velocity;
        let restitution = if input.shift {
            0.0
        } else {
            rules.body.bounciness
        };
        if x_hit {
            s.velocity[0] = -current[0] * restitution;
        }
        if z_hit {
            s.velocity[2] = -current[2] * restitution;
        }
        if vertical {
            let gravity = rules.gravity(current[1]);
            let restitution = if below {
                let on = world.block(world.on_pos(s, 0.2));
                tuff_core::world::fall::bounce_restitution(
                    on,
                    -current[1],
                    gravity,
                    input.shift,
                    restitution,
                )
            } else {
                restitution
            };
            s.velocity[1] = if restitution > 0.0 {
                marks.bounce = true;
                let portion = moved[1] / current[1];
                let drag = f64::from(modified_friction(0.98, rules.body.air_drag_modifier));
                (portion * gravity - current[1]) * (1.0 + portion * (drag - 1.0)) * restitution
            } else {
                0.0
            };
        }
    }
    let factor = f64::from(world.speed_factor(s, rules.body.efficiency));
    s.velocity[0] *= factor;
    s.velocity[2] *= factor;
}

/// The fluid update a move out of water ends with: the currents where
/// the body arrived push it again, and the eyes are looked at anew.
pub(super) fn refresh_fluids(s: &mut State, rules: &Rules, world: &World, marks: &mut Marks) {
    let fluid = world.immersion(s.position, s.pose);
    s.in_water = fluid.water > 0.0;
    if s.in_water {
        s.fall = 0.0;
        push_current(&mut s.velocity, &fluid.water_current, WATER_CURRENT_SCALE);
    }
    if fluid.lava > 0.0 {
        push_current(&mut s.velocity, &fluid.lava_current, rules.lava_current);
    }
    marks.fluid |= s.in_water || fluid.lava > 0.0;
    s.eyes = s.in_water && world.eyes_in_water(s.position, s.pose);
}
mod search;
pub(crate) use search::*;
