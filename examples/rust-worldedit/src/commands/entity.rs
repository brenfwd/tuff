// SPDX-License-Identifier: MIT
use super::{Command, Ctx, usage};
use crate::session::{say, tell};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use tuff_plugin_sdk::types::{Uuid, Vec3};
use tuff_plugin_sdk::world::{EntityRef, World};

const OPS_PER_TICK: usize = 48;
const MAX_RADIUS: i32 = 127;
const DEFAULT_RADIUS: i32 = 20;
const MAX_CLIP_ENTITIES: usize = 1024;
const MAX_SNBT: usize = 16384;

pub const COMMANDS: &[Command] = &[
    Command {
        name: "butcher",
        aliases: &[],
        usage: "[radius] [-p] [-n] [-g] [-a] [-b] [-t] [-f] [-r]",
        summary: "Kill hostile mobs around you",
        node: "worldedit.butcher",
        flags: "pngabtfr",
        run: butcher,
    },
    Command {
        name: "remove",
        aliases: &["rem", "rement"],
        usage: "<items|arrows|boats|minecarts|xp|tnt|paintings|itemframes> <radius>",
        summary: "Remove entities of a type around you",
        node: "worldedit.remove",
        flags: "",
        run: remove,
    },
];

const MONSTERS: &[&str] = &[
    "zombie",
    "husk",
    "drowned",
    "zombie_villager",
    "skeleton",
    "stray",
    "wither_skeleton",
    "bogged",
    "creeper",
    "spider",
    "cave_spider",
    "enderman",
    "endermite",
    "silverfish",
    "witch",
    "slime",
    "magma_cube",
    "ghast",
    "blaze",
    "phantom",
    "guardian",
    "elder_guardian",
    "shulker",
    "vindicator",
    "evoker",
    "pillager",
    "ravager",
    "vex",
    "hoglin",
    "zoglin",
    "piglin_brute",
    "zombified_piglin",
    "piglin",
    "warden",
    "breeze",
    "creaking",
];
const GOLEMS: &[&str] = &["iron_golem", "snow_golem", "copper_golem"];
const NPCS: &[&str] = &["villager", "wandering_trader"];
const AMBIENT: &[&str] = &["bat"];
const ANIMALS: &[&str] = &[
    "cow",
    "pig",
    "sheep",
    "chicken",
    "horse",
    "donkey",
    "mule",
    "llama",
    "trader_llama",
    "rabbit",
    "cat",
    "wolf",
    "ocelot",
    "parrot",
    "fox",
    "panda",
    "polar_bear",
    "goat",
    "turtle",
    "mooshroom",
    "camel",
    "sniffer",
    "armadillo",
    "frog",
    "axolotl",
    "squid",
    "glow_squid",
    "cod",
    "salmon",
    "pufferfish",
    "tropical_fish",
    "dolphin",
    "strider",
    "bee",
    "allay",
    "tadpole",
    "skeleton_horse",
    "zombie_horse",
    "happy_ghast",
];
const HANGING: &[&str] = &["painting", "item_frame", "glow_item_frame", "leash_knot"];

fn bare(kind: &str) -> &str {
    kind.strip_prefix("minecraft:").unwrap_or(kind)
}

#[derive(Default, Clone, Copy)]
pub struct Spare {
    pub pets: bool,
    pub npcs: bool,
    pub golems: bool,
    pub animals: bool,
    pub ambient: bool,
    pub named: bool,
    pub stands: bool,
}

impl Spare {
    fn from_flags(ctx: &Ctx) -> Spare {
        let friendly = ctx.flag('f');
        Spare {
            pets: ctx.flag('p'),
            npcs: ctx.flag('n') || friendly,
            golems: ctx.flag('g') || friendly,
            animals: ctx.flag('a') || friendly,
            ambient: ctx.flag('b') || friendly,
            named: ctx.flag('t'),
            stands: ctx.flag('r'),
        }
    }
}

fn is_named(data: &str) -> bool {
    data.contains("CustomName")
}

fn is_pet(data: &str) -> bool {
    data.contains("Owner")
}

pub fn butchered(kind: &str, data: &str, kill: Spare) -> bool {
    let kind = bare(kind);
    if is_named(data) && !kill.named {
        return false;
    }
    if is_pet(data) && !kill.pets {
        return false;
    }
    match kind {
        "armor_stand" => kill.stands,
        k if MONSTERS.contains(&k) => true,
        k if GOLEMS.contains(&k) => kill.golems,
        k if NPCS.contains(&k) => kill.npcs,
        k if AMBIENT.contains(&k) => kill.ambient,
        k if ANIMALS.contains(&k) => kill.animals,
        _ => false,
    }
}

pub fn removable(group: &str, kind: &str) -> bool {
    let kind = bare(kind);
    match group {
        "items" => kind == "item",
        "arrows" => kind == "arrow" || kind == "spectral_arrow",
        "boats" => kind.ends_with("_boat") || kind.ends_with("_raft"),
        "minecarts" => kind.ends_with("minecart"),
        "xp" => kind == "experience_orb",
        "tnt" => kind == "tnt",
        "paintings" => kind == "painting",
        "itemframes" => kind == "item_frame" || kind == "glow_item_frame",
        _ => false,
    }
}

pub fn is_group(group: &str) -> bool {
    matches!(
        group,
        "items" | "arrows" | "boats" | "minecarts" | "xp" | "tnt" | "paintings" | "itemframes"
    )
}

enum Op {
    Remove(Uuid),
    Spawn {
        dim: String,
        kind: String,
        at: Vec3,
        snbt: String,
    },
}

struct Batch {
    owner: u128,
    ops: VecDeque<Op>,
    removed: u32,
    spawned: u32,
    label: &'static str,
}

#[derive(Default)]
struct State {
    batches: VecDeque<Batch>,
    timer: bool,
    clips: HashMap<u128, Vec<ClipEntity>>,
}

#[derive(Clone)]
pub struct ClipEntity {
    kind: String,
    rel: [f64; 3],
    snbt: String,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn queue(batch: Batch) {
    let arm = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.batches.push_back(batch);
        !std::mem::replace(&mut s.timer, true)
    });
    if arm {
        tuff_plugin_sdk::schedule::after(1, pump);
    }
}

fn pump() {
    let mut budget = OPS_PER_TICK;
    let mut done = Vec::new();
    let more = STATE.with(|s| {
        let mut s = s.borrow_mut();
        while budget > 0 {
            let Some(batch) = s.batches.front_mut() else {
                break;
            };
            let Some(op) = batch.ops.pop_front() else {
                done.push(s.batches.pop_front().unwrap());
                continue;
            };
            budget -= 1;
            match op {
                Op::Remove(uuid) => {
                    if EntityRef(uuid).remove().is_ok() {
                        batch.removed += 1;
                    }
                }
                Op::Spawn {
                    dim,
                    kind,
                    at,
                    snbt,
                } => {
                    if World::new(dim).spawn(&kind, at, Some(&snbt)).is_ok() {
                        batch.spawned += 1;
                    }
                }
            }
        }
        if let Some(batch) = s.batches.pop_front_if(|b| b.ops.is_empty()) {
            done.push(batch);
        }
        let more = !s.batches.is_empty();
        s.timer = more;
        more
    });
    for batch in done {
        let text = match batch.spawned > 0 {
            true => format!(
                "[WorldEdit] {}: pasted {} entities",
                batch.label, batch.spawned
            ),
            false => format!(
                "[WorldEdit] {}: removed {} entities",
                batch.label, batch.removed
            ),
        };
        tell(batch.owner, &text);
    }
    if more {
        tuff_plugin_sdk::schedule::after(1, pump);
    }
}

fn center_and_world() -> Option<(World, Vec3)> {
    let at = tuff_plugin_sdk::world::acting::location()?;
    Some((World::new(at.dimension), at.pos))
}

fn parse_radius(text: Option<&&str>) -> Result<i32, String> {
    let Some(text) = text else {
        return Ok(DEFAULT_RADIUS);
    };
    match text.parse::<i32>() {
        Ok(r) if (1..=MAX_RADIUS).contains(&r) => Ok(r),
        _ => Err(format!("The radius must be 1 to {MAX_RADIUS}")),
    }
}

fn nearby(world: &World, c: Vec3, r: i32) -> Result<Vec<tuff_plugin_sdk::types::Entity>, String> {
    let r = f64::from(r);
    let a = Vec3 {
        x: c.x - r,
        y: c.y - r,
        z: c.z - r,
    };
    let b = Vec3 {
        x: c.x + r,
        y: c.y + r,
        z: c.z + r,
    };
    world
        .entities(a, b)
        .map_err(|e| format!("Could not list entities: {e:?}"))
}

fn butcher(ctx: &Ctx) -> i32 {
    let radius = match parse_radius(ctx.args.first()) {
        Ok(r) => r,
        Err(why) => return say(why),
    };
    let Some((world, c)) = center_and_world() else {
        return say("Only a player can butcher");
    };
    let spare = Spare::from_flags(ctx);
    match butcher_around(ctx.id, &world, c, radius, spare) {
        Ok(n) => say(format!("Butchering {n} entities")),
        Err(why) => say(why),
    }
}

pub fn butcher_around(
    owner: u128,
    world: &World,
    c: Vec3,
    radius: i32,
    spare: Spare,
) -> Result<usize, String> {
    let found = nearby(world, c, radius)?;
    let mut ops = VecDeque::new();
    for entity in found {
        let Ok(Some(data)) = EntityRef(entity.uuid).data() else {
            continue;
        };
        if butchered(&entity.kind, &data, spare) {
            ops.push_back(Op::Remove(entity.uuid));
        }
    }
    let n = ops.len();
    queue(Batch {
        owner,
        ops,
        removed: 0,
        spawned: 0,
        label: "butcher",
    });
    Ok(n)
}

fn remove(ctx: &Ctx) -> i32 {
    let Some(group) = ctx.args.first().copied().filter(|g| is_group(g)) else {
        return say(usage(&COMMANDS[1]));
    };
    let radius = match parse_radius(ctx.args.get(1)) {
        Ok(r) => r,
        Err(why) => return say(why),
    };
    let Some((world, c)) = center_and_world() else {
        return say("Only a player can remove entities");
    };
    let found = match nearby(&world, c, radius) {
        Ok(f) => f,
        Err(why) => return say(why),
    };
    let ops: VecDeque<Op> = found
        .iter()
        .filter(|e| removable(group, &e.kind))
        .map(|e| Op::Remove(e.uuid))
        .collect();
    let n = ops.len();
    queue(Batch {
        owner: ctx.id,
        ops,
        removed: 0,
        spawned: 0,
        label: "remove",
    });
    say(format!("Removing {n} entities"))
}

pub fn forget_clip(id: u128) {
    STATE.with(|s| s.borrow_mut().clips.remove(&id));
}

pub fn capture_clip(id: u128, dim: &str, min: [i32; 3], max: [i32; 3], origin: [i32; 3]) -> usize {
    let world = World::new(dim.to_owned());
    let a = Vec3 {
        x: f64::from(min[0]),
        y: f64::from(min[1]),
        z: f64::from(min[2]),
    };
    let b = Vec3 {
        x: f64::from(max[0] + 1),
        y: f64::from(max[1] + 1),
        z: f64::from(max[2] + 1),
    };
    let mut kept = Vec::new();
    let listed = world.entities(a, b);
    for e in listed.unwrap_or_default() {
        if kept.len() >= MAX_CLIP_ENTITIES || HANGING.contains(&bare(&e.kind)) {
            continue;
        }
        let Ok(Some(snbt)) = EntityRef(e.uuid).data() else {
            continue;
        };
        if snbt.len() > MAX_SNBT {
            continue;
        }
        kept.push(ClipEntity {
            kind: e.kind,
            rel: [
                e.pos.x - f64::from(origin[0]),
                e.pos.y - f64::from(origin[1]),
                e.pos.z - f64::from(origin[2]),
            ],
            snbt,
        });
    }
    let n = kept.len();
    STATE.with(|s| s.borrow_mut().clips.insert(id, kept));
    n
}

pub fn transform_clip(id: u128, map: impl Fn([f64; 3]) -> [f64; 3]) {
    STATE.with(|s| {
        if let Some(list) = s.borrow_mut().clips.get_mut(&id) {
            for e in list {
                let c = [e.rel[0] - 0.5, e.rel[1] - 0.5, e.rel[2] - 0.5];
                let r = map(c);
                e.rel = [r[0] + 0.5, r[1] + 0.5, r[2] + 0.5];
            }
        }
    });
}

pub fn paste_clip(id: u128, dim: &str, anchor: [i32; 3]) -> usize {
    let Some(list) = STATE.with(|s| s.borrow().clips.get(&id).cloned()) else {
        return 0;
    };
    let ops: VecDeque<Op> = list
        .into_iter()
        .map(|e| Op::Spawn {
            dim: dim.to_owned(),
            kind: e.kind,
            at: Vec3 {
                x: f64::from(anchor[0]) + e.rel[0],
                y: f64::from(anchor[1]) + e.rel[1],
                z: f64::from(anchor[2]) + e.rel[2],
            },
            snbt: e.snbt,
        })
        .collect();
    let n = ops.len();
    if n > 0 {
        queue(Batch {
            owner: id,
            ops,
            removed: 0,
            spawned: 0,
            label: "paste",
        });
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Spare = Spare {
        pets: false,
        npcs: false,
        golems: false,
        animals: false,
        ambient: false,
        named: false,
        stands: false,
    };

    #[test]
    fn default_butcher_spares_named_pets_and_friendlies() {
        assert!(butchered(
            "minecraft:zombie",
            "{id:\"minecraft:zombie\"}",
            NONE
        ));
        assert!(!butchered(
            "minecraft:zombie",
            "{CustomName:'\"Bob\"'}",
            NONE
        ));
        assert!(!butchered(
            "minecraft:wolf",
            "{Owner:[I;1,2,3,4]}",
            Spare {
                animals: true,
                ..NONE
            }
        ));
        assert!(!butchered("minecraft:cow", "{}", NONE));
        assert!(butchered(
            "minecraft:cow",
            "{}",
            Spare {
                animals: true,
                ..NONE
            }
        ));
        assert!(!butchered("minecraft:armor_stand", "{}", NONE));
        assert!(butchered(
            "minecraft:armor_stand",
            "{}",
            Spare {
                stands: true,
                ..NONE
            }
        ));
        assert!(butchered(
            "minecraft:zombie",
            "{CustomName:'x'}",
            Spare {
                named: true,
                ..NONE
            }
        ));
    }

    #[test]
    fn remove_groups_match_kinds() {
        assert!(removable("boats", "minecraft:oak_chest_boat"));
        assert!(removable("minecarts", "minecraft:hopper_minecart"));
        assert!(removable("itemframes", "minecraft:glow_item_frame"));
        assert!(!removable("items", "minecraft:arrow"));
    }
}
