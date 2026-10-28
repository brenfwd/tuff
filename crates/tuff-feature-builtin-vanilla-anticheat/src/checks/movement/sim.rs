//! A vanilla client walking, sprinting and jumping through a test world,
//! with the game's own collision, and the movement checks judging every
//! position it reports.

use std::collections::HashMap;
use std::sync::Arc;

use tuff_core::protocol::play::{Movement, Serverbound};
use tuff_core::world::fake::motion::{Body, Held, MotionState};
use tuff_core::world::physics::{Aabb, Movement as Attributes};
use tuff_core::world::session::mirror::ChunkMirror;
use tuff_feature_builtin_vanilla_blocks::world::fluid::VanillaFluids;
use tuff_feature_builtin_vanilla_players::{GroundMotion, Rig};
use tuff_vanilla_data::{Block, BlockState};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{Chunk, ChunkShape};
use tuff_world_model::pos::ChunkPos;

use super::super::engine::{Engine, Predicted};
use super::super::{Check, Facts, GroundSupport, facts};
use super::{AirBound, MoveEnvelope, SustainedSpeed, Terrain};

/// The period test worlds repeat with; a client straying half of it from
/// the centre is moved back by one.
pub(crate) const SPAN: i32 = 32;
const SILENT_SQ: f64 = 2.0e-4 * 2.0e-4;

pub(crate) struct World {
    chunks: HashMap<ChunkPos, Chunk>,
}

impl World {
    pub(crate) fn new() -> Self {
        let mut chunks = HashMap::new();
        let reach = (SPAN * 2) >> 4;
        for x in -reach..reach {
            for z in -reach..reach {
                chunks.insert(ChunkPos::new(x, z), Chunk::empty(ChunkShape::OVERWORLD));
            }
        }
        World { chunks }
    }

    pub(crate) fn set(&mut self, x: i32, y: i32, z: i32, state: BlockState) {
        let pos = BlockPos::new(x, y, z);
        if let Some(chunk) = self.chunks.get_mut(&ChunkPos::of_block(pos)) {
            chunk.set(x, y, z, state);
        }
    }

    /// `column(x, z)` repeated with a period of `SPAN` over the world.
    pub(crate) fn tile(&mut self, column: impl Fn(i32, i32) -> Vec<(i32, BlockState)>) {
        for x in -2 * SPAN..2 * SPAN {
            for z in -2 * SPAN..2 * SPAN {
                for (y, state) in column(x.rem_euclid(SPAN), z.rem_euclid(SPAN)) {
                    self.set(x, y, z, state);
                }
            }
        }
    }

    pub(crate) fn mirror(self) -> ChunkMirror {
        let mut mirror = ChunkMirror::default();
        for (pos, chunk) in self.chunks {
            mirror.insert(pos, Arc::new(chunk));
        }
        mirror
    }
}

pub(crate) fn stone() -> BlockState {
    Block::Stone.default_state()
}

/// Keys held for one tick.
#[derive(Clone, Copy, Default)]
pub(crate) struct Input {
    pub(crate) forward: bool,
    pub(crate) back: bool,
    pub(crate) left: bool,
    pub(crate) right: bool,
    pub(crate) sprint: bool,
    pub(crate) jump: bool,
    pub(crate) sneak: bool,
    /// Eating, which slows the input.
    pub(crate) eating: bool,
    pub(crate) yaw: f32,
    pub(crate) pitch: f32,
}

impl Input {
    /// The flags of a Player Input packet.
    pub(crate) fn flags(&self) -> u8 {
        u8::from(self.forward)
            | u8::from(self.back) << 1
            | u8::from(self.left) << 2
            | u8::from(self.right) << 3
            | u8::from(self.jump) << 4
            | u8::from(self.sneak) << 5
            | u8::from(self.sprint) << 6
    }
}

/// A vanilla client: the shared player motion, with its position
/// reporting and the settings tests change between ticks.
pub(crate) struct Client {
    pub(crate) position: [f64; 3],
    pub(crate) velocity: [f64; 3],
    pub(crate) on_ground: bool,
    pub(crate) sent: [f64; 3],
    reminder: u32,
    /// Wears leather boots.
    pub(crate) walker: bool,
    /// Under the Weaving effect.
    pub(crate) weaving: bool,
    /// Scales the walking acceleration, for cheats.
    pub(crate) boost: f64,
    /// Speed effect multiplier, on the ground only.
    pub(crate) effect: f64,
    /// Jump power, for cheats.
    pub(crate) jump_power: f32,
    /// Depth Strider's water movement efficiency.
    pub(crate) water_efficiency: f32,
    pub(crate) dolphins: bool,
    /// The Levitation amplifier.
    pub(crate) levitation: Option<u8>,
    /// Elytra flight.
    pub(crate) gliding: bool,
    /// Crawling since the last tick, which shrinks the box.
    pub(crate) prone: bool,
    /// Boxes of entities the client collides with.
    pub(crate) solids: Vec<Aabb>,
    state: MotionState,
}

impl Client {
    pub(crate) fn new(position: [f64; 3]) -> Self {
        let mut state = MotionState::new(position);
        state.on_ground = true;
        Client {
            position,
            velocity: [0.0; 3],
            on_ground: true,
            sent: position,
            reminder: 0,
            walker: false,
            weaving: false,
            boost: 1.0,
            effect: 1.0,
            jump_power: 0.42,
            water_efficiency: 0.0,
            dolphins: false,
            levitation: None,
            gliding: false,
            prone: false,
            solids: Vec::new(),
            state,
        }
    }

    /// One client tick; returns the position it reports, if any.
    pub(crate) fn tick(&mut self, world: &ChunkMirror, input: Input) -> Option<[f64; 3]> {
        let held = Held {
            forward: i8::from(input.forward) - i8::from(input.back),
            strafe: i8::from(input.left) - i8::from(input.right),
            jump: input.jump,
            jump_at_edge: false,
            sprint: input.sprint,
            sneak: input.sneak,
            yaw: input.yaw,
            pitch: input.pitch,
        };
        let body = Body {
            movement: Attributes {
                speed: 0.1,
                gravity: 0.08,
                step_height: 0.6,
                jump_strength: self.jump_power,
                water_efficiency: self.water_efficiency,
                movement_efficiency: 0.0,
                friction_modifier: 1.0,
                air_drag_modifier: 1.0,
            },
            food: 20,
            glider: false,
            boosting: false,
            fluids: &VanillaFluids,
        };
        let rig = Rig {
            driven: true,
            item_slow: input.eating,
            boost: self.boost,
            effect: self.effect,
            dolphins: self.dolphins,
            levitation: self.levitation,
            walker: self.walker,
            weaving: self.weaving,
            solids: self.solids.clone(),
        };
        let state = &mut self.state;
        state.position = self.position;
        state.velocity = self.velocity;
        state.on_ground = self.on_ground;
        state.gliding = self.gliding;
        state.swim_pose |= self.prone;
        GroundMotion.advance(state, &held, &body, &rig, world);
        self.position = state.position;
        self.velocity = state.velocity;
        self.on_ground = state.on_ground;
        self.prone = state.swim_pose || state.gliding;
        self.reminder += 1;
        let d: f64 = (0..3)
            .map(|axis| (self.position[axis] - self.sent[axis]).powi(2))
            .sum();
        (d > SILENT_SQ || self.reminder >= 20).then(|| {
            self.sent = self.position;
            self.reminder = 0;
            self.position
        })
    }

    /// Moves the client back by whole spans once it strays from the
    /// centre; the tiled world looks the same there.
    pub(crate) fn wrap(&mut self) {
        for axis in [0, 2] {
            let shift = if self.position[axis] > f64::from(SPAN / 2) {
                -f64::from(SPAN)
            } else if self.position[axis] < -f64::from(SPAN / 2) {
                f64::from(SPAN)
            } else {
                0.0
            };
            self.position[axis] += shift;
            self.sent[axis] += shift;
        }
    }
}

/// The look direction from the game's sine table.
pub(crate) fn look(yaw: f32, pitch: f32) -> [f64; 3] {
    let radians = (std::f64::consts::PI / 180.0) as f32;
    let sin = |v: f32| super::super::predict::table_sin(v * 10430.378) as f32;
    let cos = |v: f32| super::super::predict::table_sin(v * 10430.378 + 16384.0) as f32;
    let (x, y) = (pitch * radians, -yaw * radians);
    [
        f64::from(sin(y) * cos(x)),
        f64::from(-sin(x)),
        f64::from(cos(y) * cos(x)),
    ]
}

/// The movement checks fed the way the interceptor feeds them. Every
/// flag counts, corrections of a true ground claim included.
pub(crate) struct Judge {
    checks: Vec<Box<dyn Check>>,
    engine: Engine,
    keys: Option<u8>,
    pub(crate) facts: Facts,
    pub(crate) flags: Vec<(usize, String)>,
    pub(crate) moves: usize,
    pub(crate) jumps: usize,
    /// Moves the engine judged, and those it replayed exactly.
    pub(crate) judged: usize,
    pub(crate) exact: usize,
    pub(crate) misses: Vec<usize>,
    /// The highest position reported.
    pub(crate) peak: f64,
    /// Exact replays that went through water or lava.
    pub(crate) wet: usize,
    /// The tags of every judged move.
    pub(crate) tags: Vec<crate::trace::Tags>,
}

impl Judge {
    pub(crate) fn new(position: [f64; 3]) -> Self {
        let mut facts = facts();
        facts.position = position;
        Judge {
            checks: vec![
                Box::new(MoveEnvelope::default()),
                Box::new(SustainedSpeed::default()),
                Box::new(AirBound::default()),
                Box::new(GroundSupport::default()),
                Box::new(Predicted::default()),
            ],
            engine: Engine::default(),
            keys: None,
            facts,
            flags: Vec::new(),
            moves: 0,
            jumps: 0,
            judged: 0,
            exact: 0,
            misses: Vec::new(),
            peak: f64::NEG_INFINITY,
            wet: 0,
            tags: Vec::new(),
        }
    }

    /// What the client holds this tick, sent as a Player Input packet
    /// when the keys change.
    pub(crate) fn hold(&mut self, input: &Input) {
        let flags = input.flags();
        if self.keys != Some(flags) {
            self.keys = Some(flags);
            self.engine
                .observe(&self.facts, &Serverbound::PlayerInput(flags));
        }
        if self.facts.rotation != [input.yaw, input.pitch] {
            self.engine.observe(
                &self.facts,
                &Serverbound::Move(Movement {
                    horizontal_collision: false,
                    position: None,
                    rotation: Some((input.yaw, input.pitch)),
                    on_ground: false,
                }),
            );
        }
        self.facts.sprinting = input.sprint;
        self.facts.sneaking = input.sneak;
        self.facts.rotation = [input.yaw, input.pitch];
        self.facts.item_use = input.eating.then_some(super::super::ItemUse {
            item: tuff_vanilla_data::Item::Bread,
            elapsed: 0,
            remaining: Some(32),
        });
    }

    /// Follows a client the tiled world moved back by whole spans.
    pub(crate) fn wrapped(&mut self, to: [f64; 3]) {
        let by = [0, 1, 2].map(|a| to[a] - self.facts.position[a]);
        self.engine.shift(by);
        self.facts.position = to;
    }

    pub(crate) fn tick_end(&mut self) {
        self.engine
            .observe(&self.facts, &Serverbound::ClientTickEnd);
    }

    pub(crate) fn report(&mut self, world: &ChunkMirror, to: [f64; 3], on_ground: bool) {
        self.facts.terrain = Some(Terrain::probe(world, self.facts.position, to, 0.6));
        let movement = Movement {
            horizontal_collision: false,
            position: Some(to),
            rotation: None,
            on_ground,
        };
        self.facts.prediction =
            self.engine
                .judge(&self.facts, world, &VanillaFluids, &movement, to);
        if let Some(prediction) = self.facts.prediction {
            self.judged += 1;
            self.tags.push(prediction.tags);
            let exact = prediction.tags.contains(crate::trace::Tags::PREDICTED);
            self.exact += usize::from(exact);
            self.wet += usize::from(exact && prediction.tags.contains(crate::trace::Tags::FLUID));
            if !exact {
                self.misses.push(self.moves);
            }
        }
        self.peak = self.peak.max(to[1]);
        let packet = Serverbound::Move(movement);
        for check in &mut self.checks {
            if let Some(flag) = check.examine(&self.facts, &packet) {
                let trace = check.trace().map(|t| t.to_string()).unwrap_or_default();
                self.flags.push((
                    self.moves,
                    format!("{} {}: {trace}", flag.check, flag.weight),
                ));
            }
        }
        self.jumps += usize::from((to[1] - self.facts.position[1] - 0.42).abs() < 1.0e-6);
        self.facts.position = to;
        self.moves += 1;
    }
}

/// Drives a client through `ticks` ticks of `input(tick)` and returns
/// the checks' verdicts.
pub(crate) fn drive(
    world: &ChunkMirror,
    start: [f64; 3],
    ticks: usize,
    input: impl Fn(usize) -> Input,
) -> Judge {
    drive_in(world, start, ticks, input, true)
}

/// [`drive`], wrapping around the tiled world only when `wrap` (shape
/// offsets do not repeat with the tiles).
fn drive_in(
    world: &ChunkMirror,
    start: [f64; 3],
    ticks: usize,
    input: impl Fn(usize) -> Input,
    wrap: bool,
) -> Judge {
    let mut client = Client::new(start);
    let mut judge = Judge::new(start);
    for tick in 0..ticks {
        let held = input(tick);
        judge.hold(&held);
        if let Some(to) = client.tick(world, held) {
            judge.report(world, to, client.on_ground);
        }
        judge.tick_end();
        if wrap {
            let before = client.position;
            client.wrap();
            if before != client.position {
                judge.wrapped(client.sent);
            }
        }
    }
    judge
}

mod tests;
pub(crate) use tests::{
    SETTLING, assert_mostly_exact, deep, hash, liquid, plain, pools, predict_flags, roaming, scene,
    wade,
};
