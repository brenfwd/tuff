// SPDX-License-Identifier: MIT
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code, unused_imports))]
//! A generic, Baritone-like bots plugin: other plugins spawn bots (fake
//! players) through the typed `control` interface, set a process on them
//! and hear about them through `events`. The registry and process manager
//! live in [`core`], free of host calls; this file binds them to the SDK.

pub mod build;
pub mod combat;
pub mod commands;
pub mod core;
pub mod danger;
pub mod exec;
pub mod farm;
pub mod flight;
pub mod follow;
pub mod gear;
pub mod hands;
pub mod mine;
pub mod path;
pub mod pickup;
pub mod ranged;
pub mod upkeep;
pub mod world;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tuff_bots_api::{control, events};
use tuff_plugin_sdk::fake::{FakePlayer, Spawn};
use tuff_plugin_sdk::messaging::plugin_loaded;
use tuff_plugin_sdk::prelude::*;
use tuff_plugin_sdk::schedule::every;
use tuff_plugin_sdk::types::FakeError;

use crate::core::{Body, Bots, Delivery, Event, Host, Lookup, Perceived};
use crate::exec::Drive;

const MAX_NEARBY: usize = 64;

const DEFAULT_DIMENSION: &str = "minecraft:overworld";

#[derive(Default)]
struct Real {
    bodies: HashMap<Body, (FakePlayer, Perceived, String)>,
    worlds: world::host::Worlds,
    tools: HashMap<Body, Rc<RefCell<FakeTools>>>,
}

/// Break times asked of the host once per block state, until the hotbar
/// changes.
struct FakeTools {
    fake: FakePlayer,
    hotbar: Vec<(u8, String)>,
    known: HashMap<String, Option<path::BreakTool>>,
}

impl path::Tools for FakeTools {
    fn break_tool(&mut self, state: &str) -> Option<path::BreakTool> {
        if let Some(known) = self.known.get(state) {
            return *known;
        }
        let tool = self.fake.break_ticks(state).map(|t| path::BreakTool {
            ticks: t.ticks,
            slot: t.slot,
        });
        self.known.insert(state.to_owned(), tool);
        tool
    }
}

fn direction(face: exec::Face) -> tuff_plugin_sdk::types::Direction {
    use tuff_plugin_sdk::types::Direction;
    match face {
        exec::Face::Down => Direction::Down,
        exec::Face::Up => Direction::Up,
        exec::Face::North => Direction::North,
        exec::Face::South => Direction::South,
        exec::Face::West => Direction::West,
        exec::Face::East => Direction::East,
    }
}

fn body_of(fake: &FakePlayer) -> Body {
    let uuid = fake.uuid();
    (u128::from(uuid.high) << 64) | u128::from(uuid.low)
}

fn control_error(error: FakeError) -> control::ControlError {
    match error {
        FakeError::NotOwner => control::ControlError::NotOwner,
        FakeError::LimitReached => control::ControlError::LimitReached,
        FakeError::NameTaken => control::ControlError::NameTaken,
        FakeError::UnknownDimension => control::ControlError::UnknownDimension,
        FakeError::Invalid(reason) => control::ControlError::Invalid(reason),
        FakeError::UnknownEntity | FakeError::Unavailable => control::ControlError::Unavailable,
    }
}

impl Host for Real {
    fn spawn(&mut self, spec: &control::SpawnSpec) -> Result<Body, control::ControlError> {
        let at = &spec.location;
        let mut spawn = Spawn::new(&spec.name).at(
            at.dimension.as_deref().unwrap_or(DEFAULT_DIMENSION),
            at.pos.x,
            at.pos.y,
            at.pos.z,
        );
        if let (Some(yaw), Some(pitch)) = (at.yaw, at.pitch) {
            spawn = spawn.facing(yaw, pitch);
        }
        if spec.model == control::Model::Slim {
            spawn = spawn.slim();
        }
        let fake = spawn.spawn().map_err(control_error)?;
        let body = body_of(&fake);
        let first = Perceived::standing(at.pos.clone());
        let dimension = at.dimension.clone().unwrap_or(DEFAULT_DIMENSION.into());
        self.bodies.insert(body, (fake, first, dimension));
        Ok(body)
    }

    fn remove(&mut self, body: Body) {
        self.tools.remove(&body);
        if let Some((fake, _, _)) = self.bodies.remove(&body) {
            let _ = fake.remove();
        }
    }

    fn perceive(&mut self, body: Body) -> Option<Perceived> {
        let (fake, seen, dimension) = self.bodies.get_mut(&body)?;
        match fake.state() {
            Some(state) => {
                *seen = Perceived {
                    pos: control::Vec3 {
                        x: state.pos.x,
                        y: state.pos.y,
                        z: state.pos.z,
                    },
                    health: state.health,
                    alive: state.alive,
                    yaw: state.yaw,
                    on_ground: state.on_ground,
                    in_fluid: state.in_fluid,
                    food: state.food,
                    hotbar: state.hotbar,
                    selected: state.selected_slot,
                };
                dimension.clone_from(&state.dimension);
            }
            None if tuff_plugin_sdk::fake::all()
                .iter()
                .all(|f| body_of(f) != body) =>
            {
                self.bodies.remove(&body);
                return None;
            }
            None => {}
        }
        Some(seen.clone())
    }

    fn respawn(&mut self, body: Body) {
        if let Some((fake, _, _)) = self.bodies.get(&body) {
            let _ = fake.respawn();
        }
    }

    fn drive(&mut self, body: Body, drive: Drive) {
        if let Some((fake, _, _)) = self.bodies.get(&body) {
            let _ = fake.hold(tuff_plugin_sdk::fake::Input {
                forward: drive.forward,
                strafe: drive.strafe,
                jump: drive.jump,
                jump_at_edge: drive.jump_at_edge,
                sprint: drive.sprint,
                sneak: drive.sneak,
                yaw: drive.yaw,
                pitch: drive.pitch,
            });
        }
    }

    fn dig(&mut self, body: Body, pos: (i32, i32, i32), from: &control::Vec3) {
        use tuff_plugin_sdk::types::Direction;
        let Some((fake, _, _)) = self.bodies.get(&body) else {
            return;
        };
        let (dx, dy, dz) = (
            from.x - (f64::from(pos.0) + 0.5),
            from.y + 1.62 - (f64::from(pos.1) + 0.5),
            from.z - (f64::from(pos.2) + 0.5),
        );
        let face = if dx.abs() >= dy.abs() && dx.abs() >= dz.abs() {
            if dx > 0.0 {
                Direction::East
            } else {
                Direction::West
            }
        } else if dy.abs() >= dz.abs() {
            if dy > 0.0 {
                Direction::Up
            } else {
                Direction::Down
            }
        } else if dz > 0.0 {
            Direction::South
        } else {
            Direction::North
        };
        let _ = fake.dig(
            tuff_plugin_sdk::types::BlockPos {
                x: pos.0,
                y: pos.1,
                z: pos.2,
            },
            face,
        );
    }

    fn use_held(&mut self, body: Body) {
        if let Some((fake, _, _)) = self.bodies.get(&body) {
            let _ = fake.use_item(false);
        }
    }

    fn stop_using(&mut self, body: Body) {
        if let Some((fake, _, _)) = self.bodies.get(&body) {
            let _ = fake.release_use();
        }
    }

    fn select_slot(&mut self, body: Body, slot: u8) {
        if let Some((fake, _, _)) = self.bodies.get(&body) {
            let _ = fake.select_slot(slot);
        }
    }

    fn place(&mut self, body: Body, against: (i32, i32, i32), face: exec::Face) {
        self.place_at(body, against, face, 0.5 + 0.5 * face.normal().1 as f32);
    }

    fn place_at(&mut self, body: Body, against: (i32, i32, i32), face: exec::Face, cursor_y: f32) {
        let Some((fake, _, _)) = self.bodies.get(&body) else {
            return;
        };
        let (nx, ny, nz) = face.normal();
        let half = |n: i32| 0.5 + 0.5 * f64::from(n);
        let _ = fake.use_item_on(
            tuff_plugin_sdk::types::BlockPos {
                x: against.0,
                y: against.1,
                z: against.2,
            },
            direction(face),
            tuff_plugin_sdk::types::Vec3 {
                x: half(nx),
                y: if ny == 0 {
                    f64::from(cursor_y)
                } else {
                    half(ny)
                },
                z: half(nz),
            },
            false,
        );
    }

    fn inventory(&mut self, body: Body) -> Vec<hands::Carried> {
        let Some((fake, _, _)) = self.bodies.get(&body) else {
            return Vec::new();
        };
        let Some(inventory) = fake.inventory() else {
            return Vec::new();
        };
        let carried: Vec<hands::Carried> = inventory
            .slots
            .into_iter()
            .map(|s| (s.index, s.stack.item, u32::from(s.stack.count)))
            .collect();
        if let Some(tools) = self.tools.get(&body) {
            let hotbar: Vec<(u8, String)> = carried
                .iter()
                .filter(|(slot, _, _)| hands::HOTBAR.contains(slot))
                .map(|(slot, item, _)| (*slot, item.clone()))
                .collect();
            let mut tools = tools.borrow_mut();
            if tools.hotbar != hotbar {
                tools.hotbar = hotbar;
                tools.known.clear();
            }
        }
        carried
    }

    fn tools(&mut self, body: Body) -> Option<Rc<RefCell<dyn path::Tools>>> {
        let fake = self.bodies.get(&body)?.0;
        let tools = self.tools.entry(body).or_insert_with(|| {
            Rc::new(RefCell::new(FakeTools {
                fake,
                hotbar: Vec::new(),
                known: HashMap::new(),
            }))
        });
        Some(tools.clone())
    }

    fn blocks(&mut self, body: Body) -> Option<&mut dyn world::Blocks> {
        let dimension = self.bodies.get(&body)?.2.clone();
        Some(self.worlds.cache(&dimension))
    }

    fn locate(&mut self, body: Body, target: &str) -> Option<control::Vec3> {
        let dimension = &self.bodies.get(&body)?.2;
        let at = tuff_plugin_sdk::players::location(target)?;
        (&at.dimension == dimension).then_some(control::Vec3 {
            x: at.pos.x,
            y: at.pos.y,
            z: at.pos.z,
        })
    }

    fn target(&mut self, body: Body, uuid: &str) -> Lookup {
        let Some((_, _, dimension)) = self.bodies.get(&body) else {
            return Lookup::Gone;
        };
        if let Some(player) = tuff_plugin_sdk::players::state(uuid) {
            let at = player.location;
            return match &at.dimension == dimension {
                true => Lookup::Found(combat::Target {
                    pos: control::Vec3 {
                        x: at.pos.x,
                        y: at.pos.y,
                        z: at.pos.z,
                    },
                    alive: player.alive,
                    width: 0.6,
                    height: 1.8,
                }),
                false => Lookup::Gone,
            };
        }
        match tuff_plugin_sdk::entity_states(&[uuid]) {
            Err(_) => Lookup::Unknown,
            Ok(states) => match states.into_iter().next().flatten() {
                None => Lookup::Gone,
                Some(state) => Lookup::Found(combat::Target {
                    pos: control::Vec3 {
                        x: state.pos.x,
                        y: state.pos.y,
                        z: state.pos.z,
                    },
                    alive: state.health > 0.0,
                    width: 0.6,
                    height: 1.95,
                }),
            },
        }
    }

    fn attack(&mut self, body: Body, uuid: &str) {
        if let (Some((fake, _, _)), Some(target)) = (
            self.bodies.get(&body),
            tuff_plugin_sdk::types::Uuid::parse(uuid),
        ) {
            let _ = fake.attack(target);
        }
    }

    fn swing(&mut self, body: Body) {
        if let Some((fake, _, _)) = self.bodies.get(&body) {
            let _ = fake.swing();
        }
    }

    fn select(&mut self, body: Body, slot: u8) {
        if let Some((fake, _, _)) = self.bodies.get(&body) {
            let _ = fake.select_slot(slot);
        }
    }

    fn look(&mut self, body: Body, yaw: f32, pitch: f32) {
        if let Some((fake, _, _)) = self.bodies.get(&body) {
            let _ = fake.look(yaw, pitch);
        }
    }

    fn nearby(&mut self, body: Body, radius: f64) -> Vec<danger::Nearby> {
        let Some((_, seen, dimension)) = self.bodies.get(&body) else {
            return Vec::new();
        };
        let corner = |sign: f64, up: f64| tuff_plugin_sdk::types::Vec3 {
            x: seen.pos.x + sign * radius,
            y: seen.pos.y + up,
            z: seen.pos.z + sign * radius,
        };
        let Ok(found) = tuff_plugin_sdk::world::World::new(dimension.clone())
            .entities(corner(-1.0, -6.0), corner(1.0, 6.0))
        else {
            return Vec::new();
        };
        let mut found: Vec<_> = found
            .into_iter()
            .filter(|e| {
                let kind = world::name_of(&e.kind);
                kind == "item" || danger::is_hostile(kind) || danger::is_projectile(kind)
            })
            .take(MAX_NEARBY)
            .collect();
        let moving: Vec<usize> = (0..found.len())
            .filter(|i| danger::is_projectile(&found[*i].kind))
            .collect();
        let uuids: Vec<String> = moving.iter().map(|i| found[*i].uuid.to_string()).collect();
        let refs: Vec<&str> = uuids.iter().map(String::as_str).collect();
        let states = match refs.is_empty() {
            true => Vec::new(),
            false => tuff_plugin_sdk::entity_states(&refs).unwrap_or_default(),
        };
        let mut velocity = vec![control::Vec3 { x: 0.0, y: 0.0, z: 0.0 }; found.len()];
        for (i, state) in moving.iter().zip(states) {
            if let Some(state) = state {
                velocity[*i] = control::Vec3 {
                    x: state.motion.x,
                    y: state.motion.y,
                    z: state.motion.z,
                };
            }
        }
        found
            .drain(..)
            .zip(velocity)
            .map(|(e, velocity)| danger::Nearby {
                kind: e.kind,
                pos: control::Vec3 {
                    x: e.pos.x,
                    y: e.pos.y,
                    z: e.pos.z,
                },
                velocity,
            })
            .collect()
    }

    fn owner_loaded(&self, owner: &str) -> bool {
        owner == commands::OWNER || plugin_loaded(owner).is_some()
    }

    fn notify(&mut self, owner: &str, bot: control::BotId, event: Event) -> Delivery {
        if owner == commands::OWNER {
            commands::note_event(bot, &event);
            return Delivery::Sent;
        }
        let to = events::Events::at(owner);
        let sent = match event {
            Event::Spawned => to.notify_spawned(bot),
            Event::Died(cause) => to.notify_died(bot, cause),
            Event::Respawned => to.notify_respawned(bot),
            Event::ProcessDone => to.notify_process_done(bot),
            Event::ProcessFailed(reason) => to.notify_process_failed(bot, reason),
            Event::Removed => to.notify_removed(bot),
        };
        match sent {
            Ok(delivered) if delivered > 0 => Delivery::Sent,
            _ => Delivery::Gone,
        }
    }
}

#[derive(Default)]
struct State {
    bots: Bots,
    host: Real,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn with<T>(f: impl FnOnce(&mut Bots, &mut Real) -> T) -> T {
    STATE.with(|state| {
        let state = &mut *state.borrow_mut();
        f(&mut state.bots, &mut state.host)
    })
}

fn worlds<T>(f: impl FnOnce(&mut world::host::Worlds) -> T) -> T {
    STATE.with(|state| f(&mut state.borrow_mut().host.worlds))
}

struct Service;

impl control::Serve for Service {
    fn spawn(
        &mut self,
        from: &str,
        spec: control::SpawnSpec,
    ) -> Result<control::BotId, control::ControlError> {
        with(|bots, host| bots.spawn(host, from, spec))
    }

    fn remove(&mut self, from: &str, bot: control::BotId) -> Result<(), control::ControlError> {
        with(|bots, host| bots.remove(host, from, bot))
    }

    fn owned(&mut self, from: &str) -> Vec<control::BotId> {
        with(|bots, _| bots.list(from))
    }

    fn state(
        &mut self,
        from: &str,
        bot: control::BotId,
    ) -> Result<control::BotState, control::ControlError> {
        with(|bots, _| bots.state(from, bot))
    }

    fn set_skill(
        &mut self,
        from: &str,
        bot: control::BotId,
        skill: control::Skill,
    ) -> Result<(), control::ControlError> {
        with(|bots, _| bots.set_skill(from, bot, skill))
    }

    fn set_policy(
        &mut self,
        from: &str,
        bot: control::BotId,
        policy: control::Policy,
    ) -> Result<(), control::ControlError> {
        with(|bots, _| bots.set_policy(from, bot, policy))
    }

    fn set_process(
        &mut self,
        from: &str,
        bot: control::BotId,
        process: control::Process,
    ) -> Result<(), control::ControlError> {
        with(|bots, _| bots.set_process(from, bot, process))
    }

    fn stop(&mut self, from: &str, bot: control::BotId) -> Result<(), control::ControlError> {
        with(|bots, _| bots.stop(from, bot))
    }

    fn respawn(&mut self, from: &str, bot: control::BotId) -> Result<(), control::ControlError> {
        with(|bots, host| bots.respawn(host, from, bot))
    }
}

struct BotsPlugin;

impl Plugin for BotsPlugin {
    fn register() -> Result<(), String> {
        control::serve(Service).map_err(|e| format!("{e:?}"))?;
        commands::register()?;
        on::<ev::BlockBreak>(|c| {
            worlds(|w| w.block_changed(&c.dimension, &c.pos));
            with(|bots, _| bots.mark_dirty());
        })
        .map_err(|e| format!("{e:?}"))?;
        on::<ev::BlockPlace>(|c| {
            worlds(|w| w.block_changed(&c.dimension, &c.pos));
            with(|bots, _| bots.mark_dirty());
        })
        .map_err(|e| format!("{e:?}"))?;
        on::<ev::Explosion>(|e| {
            worlds(|w| w.exploded(&e.dimension, (e.center.x, e.center.y, e.center.z), e.radius));
            with(|bots, _| bots.mark_dirty());
        })
        .map_err(|e| format!("{e:?}"))
    }

    fn enable() {
        every(1, || {
            worlds(|w| w.begin_tick());
            with(|bots, host| bots.tick(host));
        });
    }
}

plugin!(BotsPlugin {
    id: "bots",
    version: "0.1.0",
    description: "Generic Baritone-like bots driven by other plugins",
    requests: [
        "cap.cross-plugin = all -- serves the bots API and notifies owners",
        "cap.player-read = all -- locates follow and attack targets",
    ],
});

#[cfg(test)]
mod tests;
