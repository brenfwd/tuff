//! Conduits: every 40 ticks they check the water and prismarine frame
//! around them, then give Conduit Power to players in water or rain within
//! range and, with a full frame, strike a hostile mob in the water nearby.

use std::collections::HashMap;

use rand::Rng;
use tuff_core::world::entity::EntityHit;
use tuff_core::world::messages::PlayerEvent;
use tuff_core::world::region_api::RegionWorld;
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::entity::MobCategory;
use tuff_vanilla_data::fluid::Fluid;
use tuff_vanilla_data::registry::{MobEffect, SoundEvent};
use tuff_vanilla_data::tag;
use tuff_world_model::BlockPos;

pub const ID: &str = "minecraft:conduit";

const CHECK_INTERVAL: u64 = 40;
const MIN_ACTIVE_SIZE: usize = 16;
const MIN_KILL_SIZE: usize = 42;
const KILL_RANGE: f64 = 8.0;
const EFFECT_TICKS: i32 = 260;

pub fn is_conduit(nbt: &Compound) -> bool {
    nbt.get_str("id") == Some(ID)
}

#[derive(Default)]
struct Conduit {
    active: bool,
    next_ambient: u64,
    target: Option<u128>,
}

#[derive(Default)]
pub struct Conduits {
    known: HashMap<BlockPos, Conduit>,
}

fn water_at(world: &dyn RegionWorld, pos: BlockPos) -> bool {
    world
        .block(pos)
        .is_some_and(|s| matches!(s.fluid_state().fluid(), Fluid::Water | Fluid::FlowingWater))
}

fn frame_size(world: &dyn RegionWorld, pos: BlockPos) -> Option<usize> {
    for x in -1..=1 {
        for y in -1..=1 {
            for z in -1..=1 {
                if !water_at(world, BlockPos::new(pos.x + x, pos.y + y, pos.z + z)) {
                    return None;
                }
            }
        }
    }
    let mut count = 0;
    for x in -2i32..=2 {
        for y in -2i32..=2 {
            for z in -2i32..=2 {
                let (ax, ay, az) = (x.abs(), y.abs(), z.abs());
                let on_frame = (ax > 1 || ay > 1 || az > 1)
                    && ((x == 0 && (ay == 2 || az == 2))
                        || (y == 0 && (ax == 2 || az == 2))
                        || (z == 0 && (ax == 2 || ay == 2)));
                if on_frame
                    && world
                        .block(BlockPos::new(pos.x + x, pos.y + y, pos.z + z))
                        .is_some_and(|s| {
                            tuff_datapack::tags::is(
                                tag::block::key::CONDUIT_EFFECT_BLOCK,
                                s.block(),
                            )
                        })
                {
                    count += 1;
                }
            }
        }
    }
    Some(count)
}

fn within(pos: BlockPos, at: [f64; 3], range: f64) -> bool {
    let dx = f64::from(pos.x) - at[0].floor();
    let dy = f64::from(pos.y) - at[1].floor();
    let dz = f64::from(pos.z) - at[2].floor();
    dx * dx + dy * dy + dz * dz < range * range
}

fn apply_effects(world: &mut dyn RegionWorld, pos: BlockPos, size: usize) {
    let range = ((size / 7) * 16) as f64;
    let height = f64::from(world.max_y() - world.min_y() + 1);
    for player in world.seen_player_details() {
        let [x, y, z] = player.position;
        if !within(pos, player.position, range) || y > f64::from(pos.y) + 1.0 + range + height {
            continue;
        }
        let feet = BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
        let wet =
            water_at(world, feet) || world.rained_on(player.position, y + player.looks.height);
        if wet {
            player.handle.send(PlayerEvent::GiveEffect {
                effect: MobEffect::ConduitPower,
                duration: EFFECT_TICKS,
                amplifier: 0,
                ambient: true,
            });
        }
    }
}

struct Candidate {
    id: i32,
    uuid: u128,
    position: [f64; 3],
}

fn wet_enemies(world: &dyn RegionWorld, pos: BlockPos) -> Vec<Candidate> {
    let mut found = Vec::new();
    for id in world.entity_ids() {
        let Some(tracked) = world.entity(id) else {
            continue;
        };
        let base = tracked.entity.base();
        let Some(living) = tracked.entity.living() else {
            continue;
        };
        let size = base.kind.size();
        let [x, y, z] = base.position;
        let half = f64::from(size.width) / 2.0;
        let near = x + half > f64::from(pos.x) - KILL_RANGE
            && x - half < f64::from(pos.x) + 1.0 + KILL_RANGE
            && z + half > f64::from(pos.z) - KILL_RANGE
            && z - half < f64::from(pos.z) + 1.0 + KILL_RANGE
            && y + f64::from(size.height) > f64::from(pos.y) - KILL_RANGE
            && y < f64::from(pos.y) + 1.0 + KILL_RANGE;
        if living.alive
            && near
            && base.kind.category() == MobCategory::Monster
            && (living.in_water || world.rained_on(base.position, y + f64::from(size.height)))
        {
            found.push(Candidate {
                id,
                uuid: base.uuid,
                position: base.position,
            });
        }
    }
    found
}

fn tracked_target(world: &dyn RegionWorld, pos: BlockPos, target: u128) -> Option<Candidate> {
    world.entity_ids().into_iter().find_map(|id| {
        let tracked = world.entity(id)?;
        let base = tracked.entity.base();
        (base.uuid == target
            && tracked.entity.living().is_some_and(|l| l.alive)
            && within(pos, base.position, KILL_RANGE))
        .then_some(Candidate {
            id,
            uuid: target,
            position: base.position,
        })
    })
}

fn update_target(world: &mut dyn RegionWorld, pos: BlockPos, conduit: &mut Conduit, hunting: bool) {
    let previous = conduit.target;
    let mut chosen = None;
    if hunting {
        chosen = match previous {
            Some(uuid) => tracked_target(world, pos, uuid),
            None => {
                let mut candidates = wet_enemies(world, pos);
                if candidates.is_empty() {
                    None
                } else {
                    let index = world.rng().gen_range(0..candidates.len());
                    Some(candidates.swap_remove(index))
                }
            }
        };
    }
    if let Some(target) = &chosen {
        world.play_sound(
            SoundEvent::BlockConduitAttackTarget,
            BlockPos::new(
                target.position[0].floor() as i32,
                target.position[1].floor() as i32,
                target.position[2].floor() as i32,
            ),
            1.0,
            1.0,
            None,
        );
        let mut hit = EntityHit::new(4.0, &damage_type::MAGIC);
        hit.position = Some([
            f64::from(pos.x) + 0.5,
            f64::from(pos.y) + 0.5,
            f64::from(pos.z) + 0.5,
        ]);
        world.hit_entity(target.id, hit);
    }
    conduit.target = chosen.map(|c| c.uuid);
    if conduit.target != previous
        && let Some(nbt) = world.raw_block_entity_mut(pos)
    {
        match conduit.target {
            Some(uuid) => {
                nbt.insert("Target", Tag::uuid(uuid));
            }
            None => {
                nbt.remove("Target");
            }
        }
        world.block_entity_changed(pos);
    }
}

pub fn tick(world: &mut dyn RegionWorld, positions: &[BlockPos]) {
    let mut known = std::mem::take(&mut world.state().get::<Conduits>().known);
    known.retain(|pos, _| positions.contains(pos));
    let now = world.tick();
    for &pos in positions {
        if !world.raw_block_entity(pos).is_some_and(is_conduit) {
            continue;
        }
        let conduit = known.entry(pos).or_default();
        if now.is_multiple_of(CHECK_INTERVAL) {
            let size = frame_size(world, pos);
            let active = size.is_some_and(|s| s >= MIN_ACTIVE_SIZE);
            if active != conduit.active {
                let sound = if active {
                    SoundEvent::BlockConduitActivate
                } else {
                    SoundEvent::BlockConduitDeactivate
                };
                world.play_sound(sound, pos, 1.0, 1.0, None);
            }
            conduit.active = active;
            if active {
                let size = size.unwrap_or(0);
                apply_effects(world, pos, size);
                update_target(world, pos, conduit, size >= MIN_KILL_SIZE);
            }
        }
        if conduit.active {
            if now.is_multiple_of(80) {
                world.play_sound(SoundEvent::BlockConduitAmbient, pos, 1.0, 1.0, None);
            }
            if now > conduit.next_ambient {
                conduit.next_ambient = now + 60 + world.rng().gen_range(0..40);
                world.play_sound(SoundEvent::BlockConduitAmbientShort, pos, 1.0, 1.0, None);
            }
        }
    }
    world.state().get::<Conduits>().known = known;
}
