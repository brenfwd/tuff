//! A survival player sprint-jumping across open ground the way a held
//! forward, sprint and jump key would: the client's own tick (zeroed small
//! velocities, jump boost, drag, ground friction) run through the game's
//! block collision against the ground heights the bot knows.

use tuff_core::protocol::play::Serverbound;
use tuff_core::world::physics::{self, Aabb, Shape};
use tuff_world_model::BlockPos;

use crate::goal::{Goal, Rng, Step};
use crate::{BotState, Terrain};

const HALF_WIDTH: f64 = 0.3f32 as f64;
const HEIGHT: f64 = 1.8f32 as f64;
const SILENT_SQ: f64 = 2.0e-4 * 2.0e-4;
const REMINDER_TICKS: u32 = 20;
const JUMP_COOLDOWN: u32 = 10;
const START_SPRINT: i32 = 1;
const MIN_TURN: u32 = 40;
const MAX_TURN: u32 = 140;

static FULL: [[f32; 6]; 1] = [[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]];
static PARTIAL: [[[f32; 6]; 1]; 16] = {
    let mut table = [[[0.0; 6]; 1]; 16];
    let mut i = 0;
    while i < 16 {
        table[i] = [[0.0, 0.0, 0.0, 1.0, i as f32 / 16.0, 1.0]];
        i += 1;
    }
    table
};
static NONE: [[f32; 6]; 0] = [];

/// The collision shape of block `pos` in the bot's picture of the world:
/// solid up to each column's floor, a two block wall where liquid lies or
/// the column is unknown, open air above.
fn shape_at(terrain: &Terrain, pos: BlockPos) -> Shape {
    let Some(column) = terrain.column(pos.x, pos.z) else {
        return Shape::from(&FULL[..]);
    };
    if column.floor == f32::MIN {
        return Shape::from(&FULL[..]);
    }
    let y = pos.y as f32;
    let floor = if column.liquid {
        column.floor + 2.0
    } else {
        column.floor
    };
    if y + 1.0 <= floor {
        Shape::from(&FULL[..])
    } else if y < floor {
        let sixteenths = ((floor - y) * 16.0) as usize;
        Shape::from(&PARTIAL[sixteenths.min(15)][..])
    } else {
        Shape::from(&NONE[..])
    }
}

fn bounds(position: [f64; 3]) -> Aabb {
    [
        position[0] - HALF_WIDTH,
        position[1],
        position[2] - HALF_WIDTH,
        position[0] + HALF_WIDTH,
        position[1] + HEIGHT,
        position[2] + HALF_WIDTH,
    ]
}

#[derive(Debug, Clone, Copy)]
struct Client {
    position: [f64; 3],
    velocity: [f64; 3],
    on_ground: bool,
    no_jump: u32,
}

impl Client {
    /// One client tick holding forward, sprint and jump facing `yaw`;
    /// returns whether a wall stopped it horizontally.
    fn tick(&mut self, terrain: &Terrain, yaw: f32) -> bool {
        self.no_jump = self.no_jump.saturating_sub(1);
        for v in &mut self.velocity {
            if v.abs() < 0.003 {
                *v = 0.0;
            }
        }
        if self.on_ground && self.no_jump == 0 {
            self.velocity[1] = self.velocity[1].max(f64::from(0.42f32));
            let angle = yaw * 0.017453292;
            self.velocity[0] += f64::from(-angle.sin()) * 0.2;
            self.velocity[2] += f64::from(angle.cos()) * 0.2;
            self.no_jump = JUMP_COOLDOWN;
        }
        let friction: f32 = if self.on_ground { 0.6 } else { 1.0 };
        let speed = if self.on_ground {
            0.13 * (0.21600002 / (friction * friction * friction))
        } else {
            0.025999999
        };
        let push = physics::input_vector([0.0, 0.0, 0.98], speed, yaw);
        for axis in 0..3 {
            self.velocity[axis] += push[axis];
        }
        let wanted = self.velocity;
        let moved =
            physics::collide_shaped(bounds(self.position), wanted, 0.6, self.on_ground, &|pos| {
                shape_at(terrain, pos)
            });
        for axis in 0..3 {
            self.position[axis] += moved[axis];
        }
        let x_hit = (moved[0] - wanted[0]).abs() >= 1.0e-5;
        let z_hit = (moved[2] - wanted[2]).abs() >= 1.0e-5;
        let y_hit = moved[1] != wanted[1];
        self.on_ground = y_hit && wanted[1] < 0.0;
        if x_hit {
            self.velocity[0] = 0.0;
        }
        if z_hit {
            self.velocity[2] = 0.0;
        }
        if y_hit {
            self.velocity[1] = 0.0;
        }
        self.velocity[1] = (self.velocity[1] - 0.08) * f64::from(0.98f32);
        let drag = f64::from(friction * 0.91);
        self.velocity[0] *= drag;
        self.velocity[2] *= drag;
        x_hit || z_hit
    }
}

/// Sprint-jumps along a heading, turning at random intervals, when a wall
/// stops it, and back toward `home` once past `radius`.
pub struct SprintJump {
    pub home: [f64; 2],
    pub radius: f64,
    rng: Rng,
    client: Option<Client>,
    teleports: u64,
    yaw: f32,
    turn_in: u32,
    sprinting: bool,
    sent: [f64; 3],
    reported: Option<[f64; 3]>,
    reminder: u32,
}

impl SprintJump {
    pub fn new(home: [f64; 2], radius: f64, seed: u64) -> Self {
        Self {
            home,
            radius,
            rng: Rng::new(seed),
            client: None,
            teleports: 0,
            yaw: 0.0,
            turn_in: 0,
            sprinting: false,
            sent: [0.0; 3],
            reported: None,
            reminder: 0,
        }
    }

    fn turn(&mut self, position: [f64; 3]) {
        let away = [position[0] - self.home[0], position[2] - self.home[1]];
        let angle = if away[0].hypot(away[1]) > self.radius {
            (-away[0]).atan2(-away[1]) + (self.rng.unit() - 0.5) * std::f64::consts::FRAC_PI_2
        } else {
            self.rng.unit() * std::f64::consts::TAU
        };
        self.yaw = (-angle.sin()).atan2(angle.cos()).to_degrees() as f32;
        self.turn_in = self.rng.range(MIN_TURN, MAX_TURN);
    }
}

impl Goal for SprintJump {
    fn tick(&mut self, state: &BotState) -> Step {
        let Some(terrain) = state.terrain.as_ref() else {
            return Step::default();
        };
        let resync = self.client.is_none()
            || state.teleports != self.teleports
            || self.reported.is_some_and(|p| p != state.position);
        if resync {
            let fresh = self.client.is_none() || state.teleports != self.teleports;
            let velocity = self.client.map_or([0.0; 3], |c| c.velocity);
            self.client = Some(Client {
                position: state.position,
                velocity: if fresh { [0.0; 3] } else { velocity },
                on_ground: false,
                no_jump: 0,
            });
            self.teleports = state.teleports;
            self.sent = state.position;
            if fresh {
                self.turn(state.position);
            }
        }
        let mut client = self.client.expect("set above");
        if terrain
            .column_at(client.position[0], client.position[2])
            .is_none()
        {
            return Step::default();
        }
        let mut step = Step::default();
        if !self.sprinting {
            self.sprinting = true;
            step.actions.push(Serverbound::PlayerCommand(START_SPRINT));
        }
        self.turn_in = self.turn_in.saturating_sub(1);
        let before = self.yaw;
        if self.turn_in == 0 {
            self.turn(client.position);
        }
        let blocked = client.tick(terrain, self.yaw);
        if blocked {
            self.turn(client.position);
        }
        if self.yaw != before {
            step.rotation = Some((self.yaw, 0.0));
        }
        self.reminder += 1;
        let d: f64 = (0..3)
            .map(|axis| (client.position[axis] - self.sent[axis]).powi(2))
            .sum();
        self.reported = None;
        if d > SILENT_SQ || self.reminder >= REMINDER_TICKS {
            self.sent = client.position;
            self.reminder = 0;
            step.position = Some(client.position);
            self.reported = Some(client.position);
        }
        step.on_ground = client.on_ground;
        self.client = Some(client);
        step
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Block;
    use tuff_wire::PacketReader;
    use tuff_world_model::chunk::{Chunk, ChunkShape};
    use tuff_world_model::pos::ChunkPos;

    /// Stone to y 63, with a two high wall on the x 12 column line.
    fn terrain() -> Terrain {
        let mut terrain = Terrain::new(-64);
        for cx in -2..2 {
            for cz in -2..2 {
                let mut chunk = Chunk::empty(ChunkShape::OVERWORLD);
                for x in 0..16 {
                    for z in 0..16 {
                        chunk.set(x, 63, z, Block::Stone.default_state());
                        if cx == 0 && x == 12 {
                            chunk.set(x, 64, z, Block::Stone.default_state());
                            chunk.set(x, 65, z, Block::Stone.default_state());
                        }
                    }
                }
                let mut r = PacketReader::new(chunk.encode(0, 0, &[]));
                r.read_int().unwrap();
                r.read_int().unwrap();
                terrain.read_chunk(ChunkPos::new(cx, cz), &mut r).unwrap();
            }
        }
        terrain
    }

    fn state(terrain: Terrain, position: [f64; 3]) -> BotState {
        BotState {
            position,
            teleports: 1,
            terrain: Some(terrain),
            ..BotState::default()
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn hops_along_flat_ground_at_sprint_jump_speed() {
        let mut goal = SprintJump::new([0.0, 0.0], 1000.0, 5);
        let mut state = state(terrain(), [0.5, 64.0, 0.5]);
        let first = goal.tick(&state).actions;
        goal.turn_in = u32::MAX;
        goal.yaw = -90.0;
        let (mut peak, mut low) = (64.0f64, 64.0f64);
        for _ in 0..40 {
            let step = goal.tick(&state);
            if let Some(p) = step.position {
                state.position = p;
            }
            let y = goal.client.unwrap().position[1];
            peak = peak.max(y);
            low = low.min(y);
        }
        assert_eq!(first, vec![Serverbound::PlayerCommand(1)]);
        let travelled = goal.client.unwrap().position[0] - 0.5;
        assert!(travelled > 6.0, "travelled {travelled}");
        assert!(
            low >= 64.0 - 1e-9 && peak > 65.2 && peak < 65.3,
            "{low} {peak}"
        );
        assert!(state.position[0] > 4.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn stops_at_a_wall_and_turns() {
        let mut goal = SprintJump::new([0.0, 0.0], 1000.0, 5);
        let mut state = state(terrain(), [8.5, 64.0, 0.5]);
        goal.tick(&state);
        goal.turn_in = u32::MAX;
        goal.yaw = -90.0;
        let mut turned = false;
        for _ in 0..120 {
            let step = goal.tick(&state);
            turned |= step.rotation.is_some();
            if let Some(p) = step.position {
                state.position = p;
            }
            assert!(goal.client.unwrap().position[0] <= 12.0 - HALF_WIDTH + 1e-6);
        }
        assert!(turned);
    }

    #[tuff_core::watchdog]
    #[test]
    fn reports_position_only_when_it_moves_or_on_reminder() {
        let mut goal = SprintJump::new([0.0, 0.0], 1000.0, 5);
        let state = state(terrain(), [0.5, 64.0, 0.5]);
        goal.tick(&state);
        goal.turn_in = u32::MAX;
        let steps: Vec<_> = (0..60).map(|_| goal.tick(&state)).collect();
        let quiet = steps.iter().filter(|s| s.position.is_none()).count();
        assert!(quiet < steps.len());
    }
}
