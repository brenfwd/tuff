//! Potent sulfur under water: a periodic geyser counts down between
//! dormant and erupting, an erupting or continuous one lifts what floats
//! in its column, and a quiet one gives nausea to those breathing its gas
//! at the surface. Players lift themselves (their client runs the same
//! push); particles and ambient sounds are client-side too.

use std::collections::HashMap;

use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::game_event::{Context, GameEvent};
use tuff_core::world::messages::PlayerEvent;
use tuff_core::world::redstone::UPDATE_ALL;
use tuff_core::world::region_api::RegionWorld;
use tuff_item::random::{RandomSource, XoroshiroRandom};
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::registry::{MobEffect, SoundEvent};
use tuff_vanilla_data::{Block, BlockState};
use tuff_world_model::BlockPos;

const STATE: &str = "potent_sulfur_state";
const COUNTDOWN: &str = "countdown";
const MAX_WATER: i32 = 4;
const GEYSER_SALT: i64 = -904_011_478;
const NAUSEA_INTERVAL: u64 = 10;
const COUNTDOWN_INTERVAL: u64 = 20;
const NAUSEA_TICKS: i64 = 80;
const GAS_RANGE_SQ: f64 = 9.0;
const LAUNCH_FORCE: f64 = 0.20000000298023224;
const BASE_LAUNCH_SPEED: f64 = 0.30000001192092896;
const PER_WATER_SPEED: f64 = 0.1;

pub fn is_potent_sulfur(nbt: &Compound) -> bool {
    nbt.get_str("id") == Some("minecraft:potent_sulfur")
}

/// The states each geyser was last seen in, to notice it starting.
#[derive(Default)]
struct Seen(HashMap<BlockPos, &'static str>);

fn state_name(state: BlockState) -> Option<&'static str> {
    match state.get(STATE)? {
        "dry" => Some("dry"),
        "wet" => Some("wet"),
        "dormant" => Some("dormant"),
        "erupting" => Some("erupting"),
        "continuous" => Some("continuous"),
        _ => None,
    }
}

fn passable(state: BlockState) -> bool {
    state.block() == Block::Water || state.collision_boxes().is_empty()
}

fn water_source(state: BlockState) -> bool {
    let fluid = state.fluid_state();
    fluid.is_source() && fluid.fluid() == tuff_vanilla_data::fluid::Fluid::Water
}

/// The first open block above the water column on `origin` (at most four
/// water blocks deep), where the gas comes out.
fn gas_source(world: &dyn RegionWorld, origin: BlockPos) -> Option<BlockPos> {
    let max_y = origin.y + MAX_WATER + 1;
    let mut pos = BlockPos::new(origin.x, origin.y + 1, origin.z);
    while pos.y <= max_y {
        let state = world.block(pos)?;
        if water_source(state) && passable(state) {
            pos.y += 1;
        } else if passable(state) {
            return Some(pos);
        } else {
            return None;
        }
    }
    None
}

fn geyser_random(world: &dyn RegionWorld, pos: BlockPos) -> impl RandomSource {
    XoroshiroRandom::new(world.seed() ^ GEYSER_SALT)
        .fork_positional()
        .at(pos.x, pos.y, pos.z)
}

/// The geyser's countdown, every second: a dormant one waits longer the
/// deeper its water, an erupting one runs about a second per block.
fn count_down(world: &mut dyn RegionWorld, pos: BlockPos, state: BlockState, source: BlockPos) {
    let dormant = state.get(STATE) == Some("dormant");
    let Some(nbt) = world.raw_block_entity(pos) else {
        return;
    };
    let mut countdown = nbt.get_int(COUNTDOWN).unwrap_or(-1);
    if countdown <= 0 {
        let water = source.y - pos.y - 1;
        let mut random = geyser_random(world, pos);
        countdown = if dormant {
            10 * (water - 1) + random.next_int_between_inclusive(15, 30)
        } else {
            random.next_int();
            (water - 1) + random.next_int_between_inclusive(1, 2)
        };
    }
    if countdown > 0 {
        countdown -= 1;
    }
    if let Some(nbt) = world.raw_block_entity_mut(pos) {
        nbt.insert(COUNTDOWN, Tag::Int(countdown));
    }
    world.mark_blocks_dirty(pos);
    if countdown != 0 {
        return;
    }
    let next = if dormant { "erupting" } else { "dormant" };
    let Some(new) = state.with(STATE, next) else {
        return;
    };
    world.set_block_flags(pos, new, UPDATE_ALL);
    if !dormant {
        world.game_event(
            GameEvent::BlockDeactivate,
            pos.center(),
            Context::block(state),
        );
    }
}

/// Lifts the entities in the open column over an erupting geyser.
fn launch(world: &mut dyn RegionWorld, pos: BlockPos, source: BlockPos) {
    let water = source.y - pos.y - 1;
    let reach = 6 * water;
    let mut height = reach;
    for i in 0..reach {
        let at = BlockPos::new(pos.x, pos.y + 1 + i, pos.z);
        if !world.block(at).is_some_and(passable) {
            height = i;
            break;
        }
    }
    let (low, high) = if height >= 1 {
        (pos.y + 1, pos.y + 1 + height)
    } else {
        (pos.y, pos.y + 2)
    };
    let column = [
        f64::from(pos.x),
        f64::from(low),
        f64::from(pos.z),
        f64::from(pos.x + 1),
        f64::from(high),
        f64::from(pos.z + 1),
    ];
    let limit = BASE_LAUNCH_SPEED + f64::from(water) * PER_WATER_SPEED;
    for id in world.entity_ids() {
        let Some(tracked) = world.entity_mut(id) else {
            continue;
        };
        let alive = tracked.entity.living().is_none_or(|l| l.alive);
        let base = tracked.entity.base_mut();
        let b = base.bounds();
        let inside = (0..3).all(|i| b[i] < column[i + 3] && b[i + 3] > column[i]);
        if !inside
            || !alive
            || base.vehicle.is_some()
            || tuff_core::world::entity_in_tag(base.kind, "not_affected_by_geysers")
            || base.velocity[1] >= limit
        {
            continue;
        }
        base.velocity[1] += LAUNCH_FORCE;
    }
}

/// Whether an eye at `eye` breathes the gas coming out at `source`: its
/// block is open, it is within three blocks, it is just above water and
/// nothing solid stands between it and the column below the gas.
fn breathes_gas(world: &dyn RegionWorld, source: BlockPos, eye: [f64; 3]) -> bool {
    let at = BlockPos::containing(eye);
    let center = source.center();
    let distance: f64 = (0..3).map(|i| (eye[i] - center[i]).powi(2)).sum();
    if !world.block(at).is_some_and(passable) || distance > GAS_RANGE_SQ {
        return false;
    }
    let below = [eye[0], eye[1] - 1.0, eye[2]];
    if !world
        .block(BlockPos::containing(below))
        .is_some_and(water_source)
    {
        return false;
    }
    let from = BlockPos::new(source.x, source.y - 1, source.z).center();
    let air = Block::Air.default_state();
    let level = |pos: BlockPos| world.block(pos).unwrap_or(air);
    tuff_core::world::raycast::cast_colliders(from, below, &level).is_none()
}

fn nausea() -> ActiveEffect {
    let mut effect = ActiveEffect::timed(MobEffect::Nausea, NAUSEA_TICKS, 0);
    effect.ambient = true;
    effect
}

/// Every half second, everything breathing the gas gets nausea.
fn gas(world: &mut dyn RegionWorld, source: BlockPos) {
    let area = [
        f64::from(source.x) - 2.5,
        f64::from(source.y),
        f64::from(source.z) - 2.5,
        f64::from(source.x) + 3.5,
        f64::from(source.y) + 1.0,
        f64::from(source.z) + 3.5,
    ];
    let overlaps = |b: [f64; 6]| (0..3).all(|i| b[i] < area[i + 3] && b[i + 3] > area[i]);
    for player in world.present_players() {
        let [x, y, z] = player.position;
        let b = [x - 0.3, y, z - 0.3, x + 0.3, y + player.height, z + 0.3];
        if overlaps(b) && breathes_gas(world, source, [x, y + player.eye_height, z]) {
            player.handle.send(PlayerEvent::GiveEffect {
                effect: MobEffect::Nausea,
                duration: NAUSEA_TICKS as i32,
                amplifier: 0,
                ambient: true,
            });
        }
    }
    for id in world.entity_ids() {
        let Some(tracked) = world.entity(id) else {
            continue;
        };
        let Some(living) = tracked.entity.living() else {
            continue;
        };
        if !living.alive || !overlaps(tracked.entity.bounds()) {
            continue;
        }
        let b = tracked.entity.bounds();
        let eye = [
            (b[0] + b[3]) / 2.0,
            b[1] + tuff_core::world::entity::eye_height(tracked.entity.base().kind, b),
            (b[2] + b[5]) / 2.0,
        ];
        if !breathes_gas(world, source, eye) {
            continue;
        }
        if let Some(tracked) = world.entity_mut(id) {
            tracked.entity.add_effects(&[nausea()]);
        }
    }
}

/// The geyser starting: the start sound, the block event clients time
/// their plume from, and the activation game event.
fn started(world: &mut dyn RegionWorld, pos: BlockPos, state: BlockState, continuous: bool) {
    world.block_event(pos, 0, 0);
    let sound = if continuous {
        SoundEvent::BlockPotentSulfurGeyserContinuousEruption
    } else {
        SoundEvent::BlockPotentSulfurGeyserEruption
    };
    world.play_sound(sound, pos, 1.0, 1.0, None);
    world.game_event(
        GameEvent::BlockActivate,
        pos.center(),
        Context::block(state),
    );
}

/// One tick of the potent sulfur blocks at `positions`.
pub fn tick(world: &mut dyn RegionWorld, positions: &[BlockPos]) {
    let now = world.tick();
    for &pos in positions {
        let Some(state) = world
            .block(pos)
            .filter(|s| s.block() == Block::PotentSulfur && world.owns(pos))
        else {
            world.state().get::<Seen>().0.remove(&pos);
            continue;
        };
        let Some(name) = state_name(state) else {
            continue;
        };
        let before = world.state().get::<Seen>().0.insert(pos, name);
        let geyser = |n: &str| matches!(n, "dormant" | "erupting");
        if geyser(name)
            && before.is_some_and(|b| !geyser(b))
            && let Some(nbt) = world.raw_block_entity_mut(pos)
        {
            nbt.insert(COUNTDOWN, Tag::Int(-1));
        }
        let active = matches!(name, "erupting" | "continuous");
        if active && before.is_some_and(|b| b != name) {
            started(world, pos, state, name == "continuous");
        }
        if name == "dry" {
            continue;
        }
        let Some(source) = gas_source(world, pos) else {
            continue;
        };
        if active {
            launch(world, pos, source);
        }
        if matches!(name, "dormant" | "erupting") && now.is_multiple_of(COUNTDOWN_INTERVAL) {
            count_down(world, pos, state, source);
        }
        if matches!(name, "wet" | "dormant") && now.is_multiple_of(NAUSEA_INTERVAL) {
            gas(world, source);
        }
    }
}
