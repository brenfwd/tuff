//! A survival player going about ordinary play near home: it walks the
//! ground with jumps and falls a real client would make, fights mobs that
//! come within reach, builds and breaks dirt next to itself, and places,
//! opens, closes and breaks chests. Hotbar slots follow
//! [`Hotbar`]; the caller hands out the items.

use tuff_core::protocol::play::{PlayerActionStatus, Serverbound};
use tuff_world_model::{BlockPos, Face};

use crate::goal::{Goal, Rng, Step, look_at};
use crate::{BotState, Terrain};

const EYE_HEIGHT: f64 = 1.62;
const WALK_SPEED: f64 = 0.2;
const JUMP_VELOCITY: f64 = 0.42;
const GRAVITY: f64 = 0.08;
const DRAG: f64 = 0.98;
const STEP_HEIGHT: f64 = 0.6;
const JUMP_HEIGHT: f64 = 1.2;
const MAX_DROP: f64 = 3.0;
const HALF_WIDTH: f64 = 0.3 - 1e-7;
const EDGE_ROOM: f64 = 0.35;
const ATTACK_REACH: f64 = 3.0;
const ATTACK_COOLDOWN: u32 = 12;
const MOB_CENTER: f64 = 0.9;
/// Ticks to break dirt and a chest by hand, with a tick to spare.
const DIRT_TICKS: u32 = 16;
const CHEST_TICKS: u32 = 76;
const CHEST_OPEN_WAIT: u32 = 20;
const CHEST_HOLD: u32 = 40;
const SWING_EVERY: u32 = 5;

/// Which hotbar slot holds what.
#[derive(Debug, Clone, Copy)]
pub struct Hotbar {
    pub weapon: i16,
    pub chest: i16,
    pub blocks: i16,
}

impl Default for Hotbar {
    fn default() -> Self {
        Self {
            weapon: 0,
            chest: 1,
            blocks: 2,
        }
    }
}

/// How often each pastime is picked when a walk ends, out of 100.
#[derive(Debug, Clone, Copy)]
pub struct Pastimes {
    pub build: u32,
    pub chest: u32,
}

impl Default for Pastimes {
    fn default() -> Self {
        Self {
            build: 30,
            chest: 15,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Body {
    position: [f64; 3],
    vy: f64,
    on_ground: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Task {
    Walk {
        ticks: u32,
    },
    Place {
        chest: bool,
    },
    Open {
        block: BlockPos,
        face: Face,
        waited: u32,
    },
    Hold {
        block: BlockPos,
        face: Face,
        ticks: u32,
    },
    Dig {
        block: BlockPos,
        face: Face,
        ticks: u32,
    },
}

pub struct Wander {
    pub home: [f64; 2],
    pub radius: f64,
    pub hotbar: Hotbar,
    pub pastimes: Pastimes,
    rng: Rng,
    body: Option<Body>,
    teleports: u64,
    heading: [f64; 2],
    task: Task,
    selected: Option<i16>,
    cooldown: u32,
    sequence: i32,
    age: u32,
}

impl Wander {
    pub fn new(home: [f64; 2], radius: f64, seed: u64) -> Self {
        Self {
            home,
            radius,
            hotbar: Hotbar::default(),
            pastimes: Pastimes::default(),
            rng: Rng::new(seed),
            body: None,
            teleports: 0,
            heading: [1.0, 0.0],
            task: Task::Walk { ticks: 0 },
            selected: None,
            cooldown: 0,
            sequence: 0,
            age: 0,
        }
    }

    fn turn(&mut self, position: [f64; 3]) {
        let away = [position[0] - self.home[0], position[2] - self.home[1]];
        let base = if away[0].hypot(away[1]) > self.radius {
            (-away[0]).atan2(-away[1]) + (self.rng.unit() - 0.5) * std::f64::consts::FRAC_PI_2
        } else {
            self.rng.unit() * std::f64::consts::TAU
        };
        self.heading = [base.sin(), base.cos()];
    }

    fn select(&mut self, slot: i16, actions: &mut Vec<Serverbound>) {
        if self.selected != Some(slot) {
            self.selected = Some(slot);
            actions.push(Serverbound::SetCarriedItem(slot));
        }
    }

    fn next_sequence(&mut self) -> i32 {
        self.sequence += 1;
        self.sequence
    }

    fn pick_task(&mut self, position: [f64; 3]) -> Task {
        let roll = self.rng.range(0, 100);
        if roll < self.pastimes.build {
            Task::Place { chest: false }
        } else if roll < self.pastimes.build + self.pastimes.chest {
            Task::Place { chest: true }
        } else {
            self.turn(position);
            Task::Walk {
                ticks: self.rng.range(40, 160),
            }
        }
    }

    /// The neighbouring column a block can go on: same flat floor as the
    /// bot's, far enough from the bot's own box.
    fn build_spot(&self, terrain: &Terrain, body: &Body) -> Option<(BlockPos, Face)> {
        let [x, y, z] = body.position;
        let (bx, bz) = (x.floor(), z.floor());
        let here = terrain.column_at(x, z)?;
        if !here.is_flat_ground() || f64::from(here.floor) != y {
            return None;
        }
        let candidates = [
            (1, 0, bx + 1.0 - x, Face::West),
            (-1, 0, x - bx, Face::East),
            (0, 1, bz + 1.0 - z, Face::North),
            (0, -1, z - bz, Face::South),
        ];
        candidates
            .into_iter()
            .filter(|(_, _, room, _)| *room >= EDGE_ROOM)
            .find_map(|(dx, dz, _, face)| {
                let (cx, cz) = (bx as i32 + dx, bz as i32 + dz);
                let column = terrain.column(cx, cz)?;
                (column.is_flat_ground() && column.floor == here.floor)
                    .then(|| (BlockPos::new(cx, y as i32, cz), face))
            })
    }

    fn target_mob(&self, state: &BotState, eye: [f64; 3]) -> Option<(i32, [f64; 3])> {
        state
            .entities
            .iter()
            .filter(|(_, seen)| seen.is_mob())
            .map(|(id, seen)| {
                let center = [
                    seen.position[0],
                    seen.position[1] + MOB_CENTER,
                    seen.position[2],
                ];
                (*id, center, distance(eye, center))
            })
            .filter(|(_, _, d)| *d <= ATTACK_REACH)
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .map(|(id, center, _)| (id, center))
    }

    fn act(&mut self, state: &BotState, body: &Body, step: &mut Step) {
        let eye = [
            body.position[0],
            body.position[1] + EYE_HEIGHT,
            body.position[2],
        ];
        let at_block = |block: BlockPos| {
            [
                f64::from(block.x) + 0.5,
                f64::from(block.y) + 0.5,
                f64::from(block.z) + 0.5,
            ]
        };
        match self.task {
            Task::Walk { .. } => {
                if self.cooldown == 0
                    && let Some((id, center)) = self.target_mob(state, eye)
                {
                    self.select(self.hotbar.weapon, &mut step.actions);
                    step.rotation = Some(look_at(eye, center));
                    step.actions.push(Serverbound::Attack(id));
                    step.actions.push(Serverbound::Punch);
                    self.cooldown = ATTACK_COOLDOWN;
                }
            }
            Task::Place { chest } => {
                let spot = state
                    .terrain
                    .as_ref()
                    .and_then(|terrain| self.build_spot(terrain, body));
                let Some((block, face)) = spot else {
                    self.task = Task::Walk { ticks: 20 };
                    return;
                };
                let slot = if chest {
                    self.hotbar.chest
                } else {
                    self.hotbar.blocks
                };
                self.select(slot, &mut step.actions);
                let below = BlockPos::new(block.x, block.y - 1, block.z);
                step.rotation = Some(look_at(
                    eye,
                    [
                        f64::from(below.x) + 0.5,
                        f64::from(below.y) + 1.0,
                        f64::from(below.z) + 0.5,
                    ],
                ));
                let sequence = self.next_sequence();
                step.actions.push(Serverbound::UseItemOn {
                    hand: 0,
                    position: below,
                    face: Face::Up,
                    cursor: [0.5, 1.0, 0.5],
                    sequence,
                });
                step.actions.push(Serverbound::Punch);
                self.task = if chest {
                    Task::Open {
                        block,
                        face,
                        waited: 0,
                    }
                } else {
                    Task::Dig {
                        block,
                        face,
                        ticks: 0,
                    }
                };
            }
            Task::Open {
                block,
                face,
                waited,
            } => {
                step.rotation = Some(look_at(eye, at_block(block)));
                if state.window.is_some() {
                    self.task = Task::Hold {
                        block,
                        face,
                        ticks: 0,
                    };
                } else if waited == 0 {
                    let sequence = self.next_sequence();
                    step.actions.push(Serverbound::UseItemOn {
                        hand: 0,
                        position: block,
                        face,
                        cursor: [0.5, 0.5, 0.5],
                        sequence,
                    });
                    self.task = Task::Open {
                        block,
                        face,
                        waited: 1,
                    };
                } else if waited >= CHEST_OPEN_WAIT {
                    self.task = Task::Dig {
                        block,
                        face,
                        ticks: 0,
                    };
                } else {
                    self.task = Task::Open {
                        block,
                        face,
                        waited: waited + 1,
                    };
                }
            }
            Task::Hold { block, face, ticks } => {
                if ticks >= CHEST_HOLD {
                    if let Some(window) = state.window {
                        step.actions.push(Serverbound::ContainerClose(window));
                    }
                    self.task = Task::Dig {
                        block,
                        face,
                        ticks: 0,
                    };
                } else {
                    self.task = Task::Hold {
                        block,
                        face,
                        ticks: ticks + 1,
                    };
                }
            }
            Task::Dig { block, face, ticks } => {
                step.rotation = Some(look_at(eye, at_block(block)));
                let needed = if self.selected == Some(self.hotbar.chest) {
                    CHEST_TICKS
                } else {
                    DIRT_TICKS
                };
                let status = if ticks == 0 {
                    Some(PlayerActionStatus::StartDestroyBlock)
                } else if ticks >= needed {
                    Some(PlayerActionStatus::StopDestroyBlock)
                } else {
                    None
                };
                if let Some(status) = status {
                    let sequence = self.next_sequence();
                    step.actions.push(Serverbound::PlayerAction {
                        status,
                        position: block,
                        face: face.id() as i8,
                        sequence,
                    });
                }
                if ticks.is_multiple_of(SWING_EVERY) {
                    step.actions.push(Serverbound::Punch);
                }
                self.task = if ticks >= needed {
                    Task::Walk {
                        ticks: self.rng.range(20, 80),
                    }
                } else {
                    Task::Dig {
                        block,
                        face,
                        ticks: ticks + 1,
                    }
                };
            }
        }
    }
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// The highest floor under the footprint of a body at `x`, `z`, or
/// `None` where the ground is unknown or covered in liquid.
fn support(terrain: &Terrain, x: f64, z: f64) -> Option<f64> {
    let mut highest = f64::MIN;
    for (ox, oz) in [
        (-HALF_WIDTH, -HALF_WIDTH),
        (HALF_WIDTH, -HALF_WIDTH),
        (-HALF_WIDTH, HALF_WIDTH),
        (HALF_WIDTH, HALF_WIDTH),
    ] {
        let column = terrain.column_at(x + ox, z + oz)?;
        if column.liquid {
            return None;
        }
        highest = highest.max(f64::from(column.floor));
    }
    Some(highest)
}

impl Body {
    /// One tick of walking `delta` horizontally; `false` if a wall, drop
    /// or liquid is in the way.
    fn advance(&mut self, terrain: &Terrain, delta: [f64; 2]) -> bool {
        let mut open = true;
        let [x, y, z] = self.position;
        if delta != [0.0, 0.0] {
            let (nx, nz) = (x + delta[0], z + delta[1]);
            match support(terrain, nx, nz) {
                Some(floor) if floor - y > JUMP_HEIGHT || y - floor > MAX_DROP => open = false,
                Some(floor) if floor <= y + 1e-9 => {
                    self.position[0] = nx;
                    self.position[2] = nz;
                }
                Some(floor) if self.on_ground && floor - y <= STEP_HEIGHT => {
                    self.position = [nx, floor, nz];
                }
                Some(_) if self.on_ground => {
                    self.vy = JUMP_VELOCITY;
                    self.on_ground = false;
                }
                Some(_) => {}
                None => open = false,
            }
        }
        let Some(floor) = support(terrain, self.position[0], self.position[2]) else {
            return open;
        };
        if self.on_ground {
            if self.position[1] > floor + 1e-6 {
                self.on_ground = false;
                self.vy = 0.0;
            } else {
                self.position[1] = floor;
                return open;
            }
        } else {
            self.position[1] += self.vy;
            self.vy = (self.vy - GRAVITY) * DRAG;
        }
        if self.position[1] <= floor {
            self.position[1] = floor;
            self.vy = 0.0;
            self.on_ground = true;
        }
        open
    }
}

impl Goal for Wander {
    fn tick(&mut self, state: &BotState) -> Step {
        self.age += 1;
        if state.teleports != self.teleports || self.body.is_none() {
            if self.body.is_some() {
                self.turn(state.position);
            }
            self.teleports = state.teleports;
            self.body = Some(Body {
                position: state.position,
                vy: 0.0,
                on_ground: false,
            });
            self.task = Task::Walk { ticks: 20 };
        }
        let Some(terrain) = state.terrain.as_ref() else {
            return Step::default();
        };
        let mut body = self.body.expect("set above");
        if terrain
            .column_at(body.position[0], body.position[2])
            .is_none()
        {
            return Step::default();
        }
        self.cooldown = self.cooldown.saturating_sub(1);
        let mut step = Step::default();
        let walking = if let Task::Walk { ticks } = self.task {
            if ticks == 0 && body.on_ground {
                self.task = self.pick_task(body.position);
            } else {
                self.task = Task::Walk {
                    ticks: ticks.saturating_sub(1),
                };
            }
            matches!(self.task, Task::Walk { .. })
        } else {
            false
        };
        let delta = if walking {
            [self.heading[0] * WALK_SPEED, self.heading[1] * WALK_SPEED]
        } else {
            [0.0, 0.0]
        };
        if !body.advance(terrain, delta) {
            self.turn(body.position);
        }
        if walking {
            let yaw = (-self.heading[0]).atan2(self.heading[1]).to_degrees() as f32;
            step.rotation = Some((yaw, 0.0));
        }
        if body.on_ground || walking {
            self.act(state, &body, &mut step);
        }
        self.body = Some(body);
        step.position = Some(body.position);
        step.on_ground = body.on_ground;
        step
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Seen;
    use tuff_vanilla_data::Block;
    use tuff_vanilla_data::registry::EntityType;
    use tuff_wire::PacketReader;
    use tuff_world_model::chunk::{Chunk, ChunkShape};
    use tuff_world_model::pos::ChunkPos;

    /// Stone up to y 63 everywhere, with a one-block step at x 8 and a
    /// pool at x 12..16.
    fn terrain() -> Terrain {
        let mut chunk = Chunk::empty(ChunkShape::OVERWORLD);
        for x in 0..16 {
            for z in 0..16 {
                chunk.set(x, 63, z, Block::Stone.default_state());
                if (8..12).contains(&x) {
                    chunk.set(x, 64, z, Block::Stone.default_state());
                }
                if x >= 12 {
                    chunk.set(x, 64, z, Block::Water.default_state());
                }
            }
        }
        let mut r = PacketReader::new(chunk.encode(0, 0, &[]));
        r.read_int().unwrap();
        r.read_int().unwrap();
        let mut terrain = Terrain::new(-64);
        terrain.read_chunk(ChunkPos::new(0, 0), &mut r).unwrap();
        terrain
    }

    fn standing(x: f64, y: f64) -> Body {
        Body {
            position: [x, y, 4.5],
            vy: 0.0,
            on_ground: true,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn walks_jumps_onto_a_step_and_stops_at_water() {
        let terrain = terrain();
        let mut body = standing(6.5, 64.0);
        let mut peak = 64.0f64;
        let mut ticks = 0;
        while body.advance(&terrain, [WALK_SPEED, 0.0]) {
            peak = peak.max(body.position[1]);
            ticks += 1;
            assert!(ticks < 200, "never reached the water");
            let floor = support(&terrain, body.position[0], body.position[2]).unwrap();
            assert!(body.position[1] >= floor, "sank into the ground");
        }
        assert!(body.on_ground);
        assert_eq!(body.position[1], 65.0);
        assert!(body.position[0] < 12.0 && body.position[0] > 11.5);
        assert!(peak > 65.0 && peak < 65.3, "jumped {peak}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn falls_off_a_ledge_with_gravity() {
        let terrain = terrain();
        let mut body = standing(7.85, 65.0);
        assert!(body.advance(&terrain, [-WALK_SPEED, 0.0]));
        let mut drops = Vec::new();
        while !body.on_ground {
            let before = body.position[1];
            body.advance(&terrain, [0.0, 0.0]);
            drops.push(before - body.position[1]);
        }
        assert_eq!(body.position[1], 64.0);
        let falling = &drops[..drops.len() - 1];
        assert!(falling.len() >= 3);
        assert!(falling.windows(2).all(|pair| pair[1] > pair[0]));
    }

    #[tuff_core::watchdog]
    #[test]
    fn builds_then_digs_the_block_it_placed() {
        let mut wander = Wander::new([4.5, 4.5], 32.0, 3);
        wander.pastimes = Pastimes {
            build: 100,
            chest: 0,
        };
        let state = BotState {
            position: [4.5, 64.0, 4.5],
            teleports: 1,
            terrain: Some(terrain()),
            ..BotState::default()
        };
        let mut actions = Vec::new();
        for _ in 0..80 {
            let step = wander.tick(&state);
            actions.extend(step.actions);
        }
        let placed = actions
            .iter()
            .position(|a| matches!(a, Serverbound::UseItemOn { face: Face::Up, .. }));
        let started = actions.iter().position(|a| {
            matches!(
                a,
                Serverbound::PlayerAction {
                    status: PlayerActionStatus::StartDestroyBlock,
                    ..
                }
            )
        });
        let stopped = actions.iter().position(|a| {
            matches!(
                a,
                Serverbound::PlayerAction {
                    status: PlayerActionStatus::StopDestroyBlock,
                    ..
                }
            )
        });
        assert!(actions.contains(&Serverbound::SetCarriedItem(2)));
        assert!(placed < started && started < stopped && placed.is_some());
    }

    #[tuff_core::watchdog]
    #[test]
    fn attacks_a_mob_in_reach_on_cooldown() {
        let mut wander = Wander::new([4.5, 4.5], 32.0, 9);
        wander.pastimes = Pastimes { build: 0, chest: 0 };
        let mut state = BotState {
            position: [4.5, 64.0, 4.5],
            teleports: 1,
            terrain: Some(terrain()),
            ..BotState::default()
        };
        state.entities.insert(
            7,
            Seen {
                kind: Some(EntityType::Zombie),
                position: [5.5, 64.0, 5.5],
            },
        );
        state.entities.insert(
            8,
            Seen {
                kind: Some(EntityType::Item),
                position: [4.6, 64.0, 4.6],
            },
        );
        let attacks: usize = (0..30)
            .map(|_| {
                let mut s = state.clone();
                s.position = wander.body.map_or(state.position, |b| b.position);
                wander
                    .tick(&s)
                    .actions
                    .iter()
                    .filter(|a| matches!(a, Serverbound::Attack(7)))
                    .count()
            })
            .sum();
        assert!((2..=3).contains(&attacks), "{attacks} attacks");
    }
}
