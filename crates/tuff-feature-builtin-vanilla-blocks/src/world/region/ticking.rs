//! Block entities kept as saved NBT that tick: beacons, conduits, bells and
//! sculk catalysts, potent sulfur and playing jukeboxes. Their positions are
//! rescanned whenever a block entity appears or goes; playing jukeboxes every
//! 20 ticks.

use tuff_core::world::region_api::{RegionHooks, RegionWorld, TickPhase};
use tuff_world_model::BlockPos;

use super::{beacon, bell, catalyst, conduit, geyser};

const RESCAN_TICKS: u64 = 20;

#[derive(Default)]
struct Found {
    generation: Option<u64>,
    beacons: Vec<BlockPos>,
    conduits: Vec<BlockPos>,
    catalysts: Vec<BlockPos>,
    geysers: Vec<BlockPos>,
}

pub struct TickingBlocks;

fn sorted(mut found: Vec<BlockPos>) -> Vec<BlockPos> {
    found.sort_unstable_by_key(|p| (p.y, p.z, p.x));
    found
}

impl RegionHooks for TickingBlocks {
    fn tick(&self, world: &mut dyn RegionWorld, phase: TickPhase) {
        if phase != TickPhase::Late {
            return;
        }
        let generation = world.block_entity_generation();
        if world.state().get::<Found>().generation != Some(generation) {
            let found = Found {
                generation: Some(generation),
                beacons: sorted(world.raw_block_entities_where(&beacon::is_beacon)),
                conduits: sorted(world.raw_block_entities_where(&conduit::is_conduit)),
                catalysts: sorted(world.raw_block_entities_where(&catalyst::is_catalyst)),
                geysers: sorted(world.raw_block_entities_where(&geyser::is_potent_sulfur)),
            };
            *world.state().get::<Found>() = found;
        }
        if world.tick().is_multiple_of(RESCAN_TICKS) {
            crate::world::jukebox::rescan(world);
        }
        let found = std::mem::take(world.state().get::<Found>());
        beacon::tick(world, &found.beacons);
        conduit::tick(world, &found.conduits);
        catalyst::tick(world, &found.catalysts);
        geyser::tick(world, &found.geysers);
        bell::tick(world);
        crate::world::jukebox::tick(world);
        *world.state().get::<Found>() = found;
    }

    fn bell_rung(&self, world: &mut dyn RegionWorld, pos: BlockPos) {
        bell::rung(world, pos);
    }

    fn mob_experience(&self, world: &mut dyn RegionWorld, position: [f64; 3], amount: i32) -> bool {
        catalyst::mob_experience(world, position, amount)
    }
}
