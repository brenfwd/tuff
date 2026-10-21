// SPDX-License-Identifier: MIT
//! The utility items: fireballs, bridge eggs and TNT that lights itself,
//! and the box rules that make explosions spare the map.

use super::*;

pub const FIRE_CHARGE: &str = "minecraft:fire_charge";
pub const EGG: &str = "minecraft:egg";
const EYE_HEIGHT: f64 = 1.62;
const FUSE_TICKS: i32 = 40;
const FIREBALL_ACCELERATION: f64 = 0.1;
const EGG_SPEED: f64 = 1.2;
const BRIDGE_TICKS: u32 = 60;
pub const BLAST_PROOF: [&str; 3] = [
    "minecraft:end_stone",
    "minecraft:glass",
    "minecraft:barrier",
];

/// A thrown bridge egg and the wool it leaves behind.
#[derive(Debug)]
pub struct Bridge {
    arena: usize,
    egg: Id,
    wool: String,
    age: u32,
}

fn heading(yaw: f32, pitch: f32) -> [f64; 3] {
    let (yaw, pitch) = ((yaw as f64).to_radians(), (pitch as f64).to_radians());
    [
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    ]
}

fn owner_tag(id: Id) -> String {
    let parts = [96, 64, 32, 0].map(|shift| (id >> shift) as u32 as i32);
    format!("[I;{},{},{},{}]", parts[0], parts[1], parts[2], parts[3])
}

fn vector(v: [f64; 3]) -> String {
    format!("[{}d,{}d,{}d]", v[0], v[1], v[2])
}

/// Launches a fireball from `eye` along `dir`, owned by `owner`.
pub(super) fn fireball(
    host: &mut dyn Host,
    dimension: &str,
    eye: [f64; 3],
    dir: [f64; 3],
    owner: Id,
) {
    let start = [eye[0] + dir[0], eye[1] + dir[1], eye[2] + dir[2]];
    let snbt = format!(
        "{{Motion:{},acceleration_power:{FIREBALL_ACCELERATION}d,Owner:{}}}",
        vector(dir.map(|c| c * FIREBALL_ACCELERATION)),
        owner_tag(owner)
    );
    host.launch(dimension, "minecraft:fireball", start, &snbt);
}

/// Primes the TNT a player placed at `at`: the block turns into a lit
/// fuse owned by the placer.
pub(super) fn prime(host: &mut dyn Host, dimension: &str, at: Pos, owner: Id) {
    host.set_block(dimension, at, "minecraft:air");
    let snbt = format!("{{fuse:{FUSE_TICKS}s,owner:{}}}", owner_tag(owner));
    host.launch(dimension, "minecraft:tnt", center(at), &snbt);
}

impl Game {
    /// A player used `item` in the air facing `yaw` and `pitch`.
    pub fn used(
        &mut self,
        host: &mut dyn Host,
        who: &Who,
        dimension: &str,
        item: &str,
        [yaw, pitch]: [f32; 2],
    ) {
        let Some(i) = self.in_dimension(dimension) else {
            return;
        };
        let arena = &self.arenas[i];
        let Phase::Running(game) = &arena.phase else {
            return;
        };
        let Some(member) = game.members.get(&who.id).filter(|m| m.life == Life::Alive) else {
            return;
        };
        let wool = format!("minecraft:{}_wool", arena.map.teams[member.team].color);
        let number = arena.number;
        let Some(at) = host.position(who.id) else {
            return;
        };
        let eye = [at[0], at[1] + EYE_HEIGHT, at[2]];
        let dir = heading(yaw, pitch);
        let owner = owner_tag(who.id);
        match item {
            FIRE_CHARGE => {
                host.take(who.id, item, 1);
                fireball(host, dimension, eye, dir, who.id);
            }
            EGG => {
                host.take(who.id, item, 1);
                let snbt = format!(
                    "{{Motion:{},Owner:{owner}}}",
                    vector(dir.map(|c| c * EGG_SPEED))
                );
                if let Some(egg) = host.launch(dimension, "minecraft:egg", eye, &snbt) {
                    self.bridges.push(Bridge {
                        arena: number,
                        egg,
                        wool,
                        age: 0,
                    });
                }
            }
            _ => {}
        }
    }

    /// Lays one wool block under every flying bridge egg.
    pub(super) fn tick_bridges(&mut self, host: &mut dyn Host) {
        let mut bridges = std::mem::take(&mut self.bridges);
        bridges.retain_mut(|bridge| {
            bridge.age += 1;
            if bridge.age > BRIDGE_TICKS {
                return false;
            }
            let Some(i) = self.arenas.iter().position(|a| a.number == bridge.arena) else {
                return false;
            };
            let arena = &mut self.arenas[i];
            let Phase::Running(game) = &mut arena.phase else {
                return false;
            };
            let Some(p) = host.entity_at(bridge.egg) else {
                return false;
            };
            let at = [
                p[0].floor() as i32,
                p[1].floor() as i32 - 1,
                p[2].floor() as i32,
            ];
            if arena.map.buildable(at)
                && !game.placed.contains(&at)
                && host.block_at(&arena.dimension, at).as_deref() == Some("minecraft:air")
            {
                game.placed.insert(at);
                host.set_block(&arena.dimension, at, &bridge.wool);
            }
            true
        });
        bridges.append(&mut self.bridges);
        self.bridges = bridges;
    }
}
