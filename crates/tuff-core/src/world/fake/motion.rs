//! What a fake player's keyboard holds and the hook that turns it into
//! client movement. Core owns the state and the packets; a feature owns
//! the physics.

use crate::world::session::mirror::ChunkMirror;
use tuff_world_model::BlockPos;

/// The keys and look a fake holds until they change, like a client's
/// keyboard and mouse.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Held {
    /// -1 back, 0 none, 1 forward.
    pub forward: i8,
    /// -1 right, 0 none, 1 left.
    pub strafe: i8,
    pub jump: bool,
    /// Jump on the tick the body would otherwise walk off its block.
    pub jump_at_edge: bool,
    pub sprint: bool,
    pub sneak: bool,
    pub yaw: f32,
    pub pitch: f32,
}

impl Held {
    /// The key flags of a Player Input packet.
    pub fn flags(&self) -> u8 {
        u8::from(self.forward > 0)
            | u8::from(self.forward < 0) << 1
            | u8::from(self.strafe > 0) << 2
            | u8::from(self.strafe < 0) << 3
            | u8::from(self.jump) << 4
            | u8::from(self.sneak) << 5
            | u8::from(self.sprint) << 6
    }
}

/// The simulated client body. The hook advances it one tick at a time.
#[derive(Debug, Clone, PartialEq)]
pub struct MotionState {
    pub position: [f64; 3],
    pub velocity: [f64; 3],
    pub on_ground: bool,
    /// A wall stopped the last move.
    pub collided: bool,
    pub fall: f64,
    pub no_jump: u32,
    pub supporting: Option<BlockPos>,
    pub sprinting: bool,
    pub crouched: bool,
    pub swimming: bool,
    /// The body is in the swimming pose (a tick behind `swimming`).
    pub swim_pose: bool,
    /// The box touched water at the start of the last tick.
    pub in_water: bool,
    pub eyes_in_water: bool,
    /// The per-axis scale the next move takes from a block that holds the
    /// body back (cobweb, berry bush, powder snow), or zero.
    pub stuck: [f64; 3],
    /// Gliding on an elytra (the fall-flying pose).
    pub gliding: bool,
    /// The jump key was down last tick, so a press needs a fresh edge.
    pub jump_down: bool,
    /// The box touched powder snow on the last move.
    pub in_snow: bool,
    /// The sneak key was down at the end of the last tick.
    pub sneaked: bool,
    /// The input is damped this tick, by crouching or crawling.
    pub slowed: bool,
}

impl MotionState {
    pub fn new(position: [f64; 3]) -> Self {
        Self {
            position,
            velocity: [0.0; 3],
            on_ground: false,
            collided: false,
            fall: 0.0,
            no_jump: 0,
            supporting: None,
            sprinting: false,
            crouched: false,
            swimming: false,
            swim_pose: false,
            in_water: false,
            eyes_in_water: false,
            stuck: [0.0; 3],
            gliding: false,
            jump_down: false,
            in_snow: false,
            sneaked: false,
            slowed: false,
        }
    }
}

/// What the session knows that movement reads.
#[derive(Clone, Copy)]
pub struct Body<'a> {
    pub movement: crate::world::physics::Movement,
    pub food: i32,
    /// The chest slot carries an elytra that can still glide.
    pub glider: bool,
    /// An attached firework rocket is boosting the glide this tick.
    pub boosting: bool,
    /// How fluids flow, for currents.
    pub fluids: &'a dyn crate::world::fluid_api::FluidBehaviors,
}

/// Client physics for fake players.
pub trait FakeMotion: Send + Sync {
    /// Advances `state` by one client tick under `held`.
    fn tick(&self, state: &mut MotionState, held: &Held, body: &Body, world: &ChunkMirror);
}
