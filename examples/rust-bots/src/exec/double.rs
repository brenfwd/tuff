// SPDX-License-Identifier: MIT
//! A host-free movement double: walking, jumping and falling over a
//! [`Terrain`], close enough to the real body to test the executor.

use std::cell::RefCell;
use std::rc::Rc;

use tuff_bots_api::control::{BotId, ControlError, SpawnSpec, Vec3};

use super::{Drive, Face};
use crate::core::{Body, Delivery, Event, Host, Perceived};
use crate::hands::Carried;
use crate::path::{Pos, Tools};
use crate::world::tests::{TableTools, Terrain};
use crate::world::{Blocks, Class};

const WALK: f64 = 0.2158;
const SPRINT: f64 = 0.2806;
const GRAVITY: f64 = 0.08;
const JUMP: f64 = 0.42;
const SNEAK: f64 = 0.3;
const HALF_WIDTH: f64 = 0.3;
const CLIMB: f64 = 0.2;
const CLIMB_FALL: f64 = 0.15;

pub struct Double {
    pub terrain: Rc<RefCell<Terrain>>,
    pub pos: Vec3,
    pub yaw: f32,
    pub vy: f64,
    pub on_ground: bool,
    pub input: Drive,
    pub drives: usize,
    pub events: Vec<(BotId, Event)>,
    pub health: f32,
    pub alive: bool,
    view: SharedTerrain,
    pub fell: f64,
    pub worst_fall: f64,
    pub target: Option<Vec3>,
    pub digs: Vec<(i32, i32, i32)>,
    /// Hotbar slot (0 to 8), item and count.
    pub hotbar: Vec<(u8, String, u32)>,
    pub selected: u8,
    pub placed: Vec<Pos>,
    pub tools: Option<Rc<RefCell<TableTools>>>,
    pub lowest: f64,
    /// Physics ticks the state a plugin tick reads is behind the body.
    pub lag: usize,
    history: std::collections::VecDeque<(Vec3, bool)>,
}

struct SharedTerrain(Rc<RefCell<Terrain>>);

impl Blocks for SharedTerrain {
    fn class_at(&mut self, x: i32, y: i32, z: i32) -> Result<Class, crate::world::Pending> {
        Ok(self.0.borrow().class((x, y, z)))
    }

    fn state_at(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
    ) -> Result<Option<Rc<str>>, crate::world::Pending> {
        Ok(self.0.borrow().state((x, y, z)))
    }

    fn find(
        &mut self,
        key: crate::world::SectionKey,
        _: &[String],
    ) -> Result<Vec<(i32, i32, i32)>, crate::world::Pending> {
        Ok(self.0.borrow().ores_in(key))
    }
}

impl Double {
    pub fn new(terrain: Terrain, at: (f64, f64, f64)) -> Self {
        let terrain = Rc::new(RefCell::new(terrain));
        Self {
            view: SharedTerrain(terrain.clone()),
            terrain,
            pos: Vec3 {
                x: at.0,
                y: at.1,
                z: at.2,
            },
            yaw: 0.0,
            vy: 0.0,
            on_ground: true,
            input: Drive::default(),
            drives: 0,
            events: Vec::new(),
            health: 20.0,
            alive: true,
            fell: 0.0,
            worst_fall: 0.0,
            target: None,
            digs: Vec::new(),
            hotbar: Vec::new(),
            selected: 0,
            placed: Vec::new(),
            tools: None,
            lowest: at.1,
            lag: 0,
            history: std::collections::VecDeque::new(),
        }
    }

    fn grounded(&self, x: f64, z: f64) -> bool {
        let y = self.pos.y - 0.5;
        [-HALF_WIDTH, HALF_WIDTH].iter().any(|ox| {
            [-HALF_WIDTH, HALF_WIDTH]
                .iter()
                .any(|oz| self.class(x + ox, y, z + oz).standable())
        })
    }

    /// A step a sneaking body on the ground refuses: one off an edge.
    fn edge_holds(&self, x: f64, z: f64) -> bool {
        !(self.input.sneak && self.on_ground) || self.grounded(x, z)
    }

    pub fn carried(&self, item: &str) -> u32 {
        self.hotbar
            .iter()
            .filter(|(_, i, _)| i == item)
            .map(|(_, _, c)| c)
            .sum()
    }

    fn class(&self, x: f64, y: f64, z: f64) -> Class {
        self.terrain
            .borrow()
            .class((x.floor() as i32, y.floor() as i32, z.floor() as i32))
    }

    fn free(&self, x: f64, y: f64, z: f64) -> bool {
        self.class(x, y + 1e-3, z).passable() && self.class(x, y + 1.0, z).passable()
    }

    pub fn push(&mut self, dx: f64, dz: f64) {
        self.pos.x += dx;
        self.pos.z += dz;
    }

    /// One tick of the body under the held input.
    pub fn physics(&mut self) {
        self.yaw = self.input.yaw;
        if self.input.forward != 0 {
            let speed = if self.input.sneak {
                WALK * SNEAK
            } else if self.input.sprint {
                SPRINT
            } else {
                WALK
            } * f64::from(self.input.forward);
            let r = f64::from(self.yaw).to_radians();
            let (dx, dz) = (-r.sin() * speed, r.cos() * speed);
            if self.free(self.pos.x + dx, self.pos.y, self.pos.z)
                && self.edge_holds(self.pos.x + dx, self.pos.z)
            {
                self.pos.x += dx;
            }
            if self.free(self.pos.x, self.pos.y, self.pos.z + dz)
                && self.edge_holds(self.pos.x, self.pos.z + dz)
            {
                self.pos.z += dz;
            }
        }
        if self
            .class(self.pos.x, self.pos.y + 1e-3, self.pos.z)
            .climbable()
        {
            self.vy = if self.input.jump {
                CLIMB
            } else if self.input.sneak {
                0.0
            } else {
                self.vy.max(-CLIMB_FALL)
            };
            self.on_ground = false;
        } else if self.on_ground && self.input.jump {
            self.vy = JUMP;
            self.on_ground = false;
        }
        let ny = self.pos.y + self.vy;
        let rising = self.vy > 0.0;
        self.vy = (self.vy - GRAVITY) * 0.98;
        if rising {
            if self.class(self.pos.x, ny + 1.8, self.pos.z).passable() {
                self.pos.y = ny;
            } else {
                self.vy = -GRAVITY;
            }
            self.on_ground = false;
            self.fell = 0.0;
        } else {
            let held = [-HALF_WIDTH, HALF_WIDTH].iter().any(|ox| {
                [-HALF_WIDTH, HALF_WIDTH].iter().any(|oz| {
                    !self
                        .class(self.pos.x + ox, ny - 1e-6, self.pos.z + oz)
                        .passable()
                })
            });
            if !held {
                self.fell += self.pos.y - ny;
                self.pos.y = ny;
                self.on_ground = false;
            } else {
                self.pos.y = (ny - 1e-6).floor() + 1.0;
                self.vy = 0.0;
                self.on_ground = true;
                self.worst_fall = self.worst_fall.max(self.fell);
                self.fell = 0.0;
            }
        }
        self.lowest = self.lowest.min(self.pos.y);
        self.history.push_back((self.pos.clone(), self.on_ground));
        if self.history.len() > self.lag + 1 {
            self.history.pop_front();
        }
    }
}

impl Host for Double {
    fn spawn(&mut self, _: &SpawnSpec) -> Result<Body, ControlError> {
        Ok(1)
    }

    fn remove(&mut self, _: Body) {}

    fn perceive(&mut self, _: Body) -> Option<Perceived> {
        let (pos, on_ground) = match self.history.front() {
            Some((pos, on_ground)) if self.lag > 0 && self.history.len() > self.lag => {
                (pos.clone(), *on_ground)
            }
            _ => (self.pos.clone(), self.on_ground),
        };
        Some(Perceived {
            pos,
            health: self.health,
            alive: self.alive,
            yaw: self.yaw,
            on_ground,
            in_fluid: false,
            food: 20,
            hotbar: Vec::new(),
            selected: 0,
        })
    }

    fn respawn(&mut self, _: Body) {}

    fn drive(&mut self, _: Body, drive: Drive) {
        self.drives += 1;
        self.input = drive;
    }

    fn blocks(&mut self, _: Body) -> Option<&mut dyn Blocks> {
        Some(&mut self.view)
    }

    fn dig(&mut self, _: Body, pos: (i32, i32, i32), _: &Vec3) {
        self.digs.push(pos);
        self.terrain.borrow_mut().dig(pos);
    }

    fn select_slot(&mut self, _: Body, slot: u8) {
        self.selected = slot;
    }

    fn place(&mut self, _: Body, against: Pos, face: Face) {
        let (dx, dy, dz) = face.normal();
        let at = (against.0 + dx, against.1 + dy, against.2 + dz);
        let Some(held) = self
            .hotbar
            .iter_mut()
            .find(|(slot, _, count)| *slot == self.selected && *count > 0)
        else {
            return;
        };
        let mut terrain = self.terrain.borrow_mut();
        let body = (
            self.pos.x.floor() as i32,
            self.pos.y.floor() as i32,
            self.pos.z.floor() as i32,
        );
        let inside = at == body || at == (body.0, body.1 + 1, body.2);
        if !terrain.class(against).standable() || !terrain.class(at).passable() || inside {
            return;
        }
        held.2 -= 1;
        let name: &'static str = Box::leak(held.1.clone().into_boxed_str());
        terrain.block(at, name, Class::STANDABLE);
        self.placed.push(at);
    }

    fn inventory(&mut self, _: Body) -> Vec<Carried> {
        self.hotbar
            .iter()
            .filter(|(_, _, count)| *count > 0)
            .map(|(slot, item, count)| (36 + slot, item.clone(), *count))
            .collect()
    }

    fn tools(&mut self, _: Body) -> Option<Rc<RefCell<dyn Tools>>> {
        self.tools.clone().map(|t| t as Rc<RefCell<dyn Tools>>)
    }

    fn locate(&mut self, _: Body, _: &str) -> Option<Vec3> {
        self.target.clone()
    }

    fn owner_loaded(&self, _: &str) -> bool {
        true
    }

    fn notify(&mut self, _: &str, bot: BotId, event: Event) -> Delivery {
        self.events.push((bot, event));
        Delivery::Sent
    }
}
