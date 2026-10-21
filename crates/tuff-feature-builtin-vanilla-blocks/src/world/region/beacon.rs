//! Beacons: a column scan for a clear beam, the pyramid under it, and the
//! effects it gives players in range every 80 ticks. Saved NBT keeps the
//! pyramid `Levels` and the chosen effects; the scan state is runtime only.

use std::collections::HashMap;

use tuff_core::world::beacon::BEACON_EFFECTS;
use tuff_core::world::messages::{AdvancementEvent, PlayerEvent};
use tuff_core::world::region_api::RegionWorld;
use tuff_nbt::Compound;
use tuff_vanilla_data::registry::{MobEffect, SoundEvent};
use tuff_vanilla_data::{Block, tag};
use tuff_world_model::BlockPos;

pub const ID: &str = "minecraft:beacon";

const BLOCKS_PER_TICK: i32 = 10;
const MAX_LEVELS: i32 = 4;
const EFFECT_INTERVAL: u64 = 80;

pub fn is_beacon(nbt: &Compound) -> bool {
    nbt.get_str("id") == Some(ID)
}

struct Scan {
    last_check_y: i32,
    checking_open: bool,
    beam_open: bool,
}

#[derive(Default)]
pub struct Beacons {
    scans: HashMap<BlockPos, Scan>,
}

fn beam_block(block: Block) -> bool {
    block == Block::Beacon || {
        let name = block.name();
        name.ends_with("_stained_glass") || name.ends_with("_stained_glass_pane")
    }
}

fn effect(nbt: &Compound, key: &str) -> Option<MobEffect> {
    let effect = MobEffect::from_name(nbt.get_str(key)?)?;
    BEACON_EFFECTS
        .iter()
        .any(|tier| tier.contains(&effect))
        .then_some(effect)
}

fn pyramid_levels(world: &dyn RegionWorld, pos: BlockPos) -> i32 {
    let min_y = world.min_y();
    let mut levels = 0;
    for step in 1..=MAX_LEVELS {
        let y = pos.y - step;
        if y < min_y {
            break;
        }
        let complete = (pos.x - step..=pos.x + step).all(|x| {
            (pos.z - step..=pos.z + step).all(|z| {
                world.block(BlockPos::new(x, y, z)).is_some_and(|s| {
                    tuff_datapack::tags::is(tag::block::key::BEACON_BASE_BLOCKS, s.block())
                })
            })
        });
        if !complete {
            break;
        }
        levels = step;
    }
    levels
}

fn apply_effects(world: &mut dyn RegionWorld, pos: BlockPos, levels: i32, nbt: &Compound) {
    let Some(primary) = effect(nbt, "primary_effect") else {
        return;
    };
    let secondary = effect(nbt, "secondary_effect");
    let range = f64::from(levels * 10 + 10);
    let amplifier = u8::from(levels >= MAX_LEVELS && secondary == Some(primary));
    let duration = (9 + levels * 2) * 20;
    let height = f64::from(world.max_y() - world.min_y() + 1);
    let (bx, by, bz) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    for player in world.seen_player_details() {
        let [x, y, z] = player.position;
        let top = y + player.looks.height;
        let inside = x + 0.3 > bx - range
            && x - 0.3 < bx + 1.0 + range
            && z + 0.3 > bz - range
            && z - 0.3 < bz + 1.0 + range
            && top > by - range
            && y < by + 1.0 + range + height;
        if !inside {
            continue;
        }
        player.handle.send(PlayerEvent::GiveEffect {
            effect: primary,
            duration,
            amplifier,
            ambient: true,
        });
        if levels >= MAX_LEVELS
            && let Some(secondary) = secondary.filter(|s| *s != primary)
        {
            player.handle.send(PlayerEvent::GiveEffect {
                effect: secondary,
                duration,
                amplifier: 0,
                ambient: true,
            });
        }
    }
}

/// Tells the players within 10 blocks sideways and from 9 below to 5
/// above the beacon that it came on.
fn report_constructed(world: &mut dyn RegionWorld, pos: BlockPos, levels: i32) {
    let (bx, by, bz) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    for player in world.seen_player_details() {
        let [x, y, z] = player.position;
        let inside = x + 0.3 > bx - 10.0
            && x - 0.3 < bx + 10.0
            && z + 0.3 > bz - 10.0
            && z - 0.3 < bz + 10.0
            && y + player.looks.height > by - 9.0
            && y < by + 5.0;
        if inside {
            player.handle.send(PlayerEvent::Advancement(Box::new(
                AdvancementEvent::ConstructBeacon(levels),
            )));
        }
    }
}

fn sound(world: &dyn RegionWorld, pos: BlockPos, sound: SoundEvent) {
    world.play_sound(sound, pos, 1.0, 1.0, None);
}

pub fn tick(world: &mut dyn RegionWorld, positions: &[BlockPos]) {
    let mut scans = std::mem::take(&mut world.state().get::<Beacons>().scans);
    scans.retain(|pos, _| {
        let alive = positions.contains(pos);
        if !alive {
            sound(world, *pos, SoundEvent::BlockBeaconDeactivate);
        }
        alive
    });
    for &pos in positions {
        tick_one(world, &mut scans, pos);
    }
    world.state().get::<Beacons>().scans = scans;
}

fn tick_one(world: &mut dyn RegionWorld, scans: &mut HashMap<BlockPos, Scan>, pos: BlockPos) {
    let Some(nbt) = world
        .raw_block_entity(pos)
        .filter(|n| is_beacon(n))
        .cloned()
    else {
        return;
    };
    let Some(top) = world.world_surface(pos.x, pos.z) else {
        return;
    };
    let min_y = world.min_y();
    let scan = scans.entry(pos).or_insert(Scan {
        last_check_y: min_y - 1,
        checking_open: false,
        beam_open: false,
    });
    let mut check_y = if scan.last_check_y < pos.y {
        scan.checking_open = false;
        scan.last_check_y = pos.y - 1;
        pos.y
    } else {
        scan.last_check_y + 1
    };
    let mut i = 0;
    while i < BLOCKS_PER_TICK && check_y <= top {
        let state = world
            .block(BlockPos::new(pos.x, check_y, pos.z))
            .unwrap_or(Block::Air.default_state());
        let passes =
            scan.checking_open && (state.light_dampening() < 15 || state.block() == Block::Bedrock);
        if beam_block(state.block()) {
            scan.checking_open = true;
        } else if !passes {
            scan.checking_open = false;
            scan.last_check_y = top;
            break;
        }
        check_y += 1;
        scan.last_check_y += 1;
        i += 1;
    }
    let saved = nbt.get_int("Levels").unwrap_or(0);
    let mut levels = saved;
    if world.tick().is_multiple_of(EFFECT_INTERVAL) {
        if scan.beam_open {
            levels = pyramid_levels(world, pos);
        }
        if levels > 0 && scan.beam_open {
            apply_effects(world, pos, levels, &nbt);
            sound(world, pos, SoundEvent::BlockBeaconAmbient);
        }
    }
    if scan.last_check_y >= top {
        scan.last_check_y = min_y - 1;
        scan.beam_open = scan.checking_open;
        let (was, is) = (saved > 0, levels > 0);
        if !was && is {
            sound(world, pos, SoundEvent::BlockBeaconActivate);
            report_constructed(world, pos, levels);
        } else if was && !is {
            sound(world, pos, SoundEvent::BlockBeaconDeactivate);
        }
    }
    if levels != saved
        && let Some(nbt) = world.raw_block_entity_mut(pos)
    {
        nbt.insert("Levels", levels);
        world.block_entity_changed(pos);
    }
}
