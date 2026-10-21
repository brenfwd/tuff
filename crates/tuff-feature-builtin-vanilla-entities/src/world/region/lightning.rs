//! Rolling for lightning strikes: while a dimension is thundering, each
//! ticking chunk gets vanilla's per-tick roll; a hit picks a random x/z in
//! the chunk, the surface height there (preferring a living thing that can
//! see the sky), and strikes if it would actually be raining at that spot.
//! What a strike does lives in [`tuff_core::world::lightning`].

use rand::Rng;

use crate::world::lightning::{self, LightningBolt};
use tuff_core::world::messages::RegionInternal;
use tuff_core::world::region_api::{RegionHooks, RegionTask, RegionWorld, TickPhase};
use tuff_vanilla_data::registry::PointOfInterestType;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::{ChunkPos, REGION_CHUNKS, RegionPos};

/// How far from the target column vanilla's living-thing search reaches,
/// horizontally.
const TARGET_RADIUS: i32 = 3;
/// How far a lightning rod draws strikes from.
const ROD_RADIUS: i32 = 128;

/// Lightning's region hook: rolls strikes while thundering.
pub struct Thunder;

impl RegionHooks for Thunder {
    fn tick(&self, world: &mut dyn RegionWorld, phase: TickPhase) {
        if phase == TickPhase::Weather {
            run_thunder(world);
        }
    }
}

/// Rolls a strike for every ticking chunk while this dimension is
/// thundering.
fn run_thunder(world: &mut dyn RegionWorld) {
    if !world.is_thundering() {
        return;
    }
    for chunk in world.lit_ready_chunks() {
        if !lightning::should_strike(world.rng()) {
            continue;
        }
        let (min_x, min_z) = (chunk.x << 4, chunk.z << 4);
        let pos = world.random_block_pos(min_x, 0, min_z, 15);
        let Some(surface_y) = world.motion_blocking_height(pos.x, pos.z) else {
            continue;
        };
        let min_y = world.dimension_info().shape.min_y;
        let surface_y = if surface_y <= min_y {
            min_y + 2
        } else {
            surface_y
        };
        strike_rolled(world, BlockPos::new(pos.x, surface_y, pos.z));
    }
}

/// A strike rolled over `center`: it goes to the closest lightning rod
/// within reach, asking the neighbouring regions the reach covers in
/// turn, or else near `center`, if it rains where it lands.
pub fn strike_rolled(world: &mut dyn RegionWorld, center: BlockPos) {
    let own = closest_rod(world, center);
    let pending = rod_regions(world, center, own);
    if let Some((_, rod)) = own
        && pending.is_empty()
    {
        strike_where_owned(world, above(rod));
        return;
    }
    let candidates = sky_visible_targets(world, center);
    let fallback = lightning::pick_target(center, &candidates, world.rng());
    relay(
        world,
        Relay {
            center,
            best: own,
            fallback,
            pending,
        },
    );
}

type Rod = (i64, BlockPos);

/// A rolled strike looking for the closest lightning rod across the
/// regions its reach covers, each region adding its own rods in turn.
struct Relay {
    center: BlockPos,
    best: Option<Rod>,
    /// Where the strike lands with no rod, picked by the rolling region.
    fallback: BlockPos,
    pending: Vec<RegionPos>,
}

fn above(pos: BlockPos) -> BlockPos {
    BlockPos::new(pos.x, pos.y + 1, pos.z)
}

fn distance(a: BlockPos, b: BlockPos) -> i64 {
    let d = |a: i32, b: i32| i64::from(a - b).pow(2);
    d(a.x, b.x) + d(a.y, b.y) + d(a.z, b.z)
}

fn closer(a: Rod, b: Rod) -> bool {
    (a.0, a.1.x, a.1.y, a.1.z) < (b.0, b.1.x, b.1.y, b.1.z)
}

/// The lightning rod this region owns nearest `center` (within the
/// game's square reach) that tops its column, with its squared distance.
fn closest_rod(world: &dyn RegionWorld, center: BlockPos) -> Option<Rod> {
    world
        .pois_near(center, ROD_RADIUS, PointOfInterestType::LightningRod)
        .into_iter()
        .filter(|rod| world.owns(*rod))
        .find(|rod| world.world_surface(rod.x, rod.z) == Some(rod.y + 1))
        .map(|rod| (distance(rod, center), rod))
}

/// The other regions the rod reach around `center` overlaps that could
/// hold a rod closer than `best`.
fn rod_regions(world: &dyn RegionWorld, center: BlockPos, best: Option<Rod>) -> Vec<RegionPos> {
    let here = world.region_pos();
    let span = REGION_CHUNKS * 16;
    let corner = |d: i32| ChunkPos::of_block(BlockPos::new(center.x + d, 0, center.z + d)).region();
    let (low, high) = (corner(-ROD_RADIUS), corner(ROD_RADIUS));
    let gap = |c: i32, low: i32| i64::from((low - c).max(c - (low + span - 1)).max(0));
    let mut regions = Vec::new();
    for x in low.x..=high.x {
        for z in low.z..=high.z {
            let region = RegionPos::new(x, z);
            let near = gap(center.x, x * span).pow(2) + gap(center.z, z * span).pow(2);
            if region != here && best.is_none_or(|(d, _)| near <= d) {
                regions.push(region);
            }
        }
    }
    regions
}

/// Adds the next region's rods to the relay there, or strikes once every
/// region was asked.
fn relay(world: &mut dyn RegionWorld, mut relay: Relay) {
    while let Some(next) = relay.pending.pop() {
        let Some(handle) = world.neighbor(next) else {
            continue;
        };
        handle.internal(RegionInternal::Run(RegionTask::new(move |there| {
            if let Some(rod) = closest_rod(there, relay.center)
                && relay.best.is_none_or(|best| closer(rod, best))
            {
                relay.best = Some(rod);
            }
            self::relay(there, relay);
        })));
        return;
    }
    let target = relay.best.map_or(relay.fallback, |(_, rod)| above(rod));
    strike_where_owned(world, target);
}

/// Strikes `target` if it rains there, on the region owning it.
fn strike_where_owned(world: &mut dyn RegionWorld, target: BlockPos) {
    if world.owns(target) {
        if world
            .shared()
            .growth_behaviors
            .clone()
            .rains_at(&*world, target)
        {
            strike_lightning(world, target);
        }
        return;
    }
    if let Some(owner) = world.neighbor(ChunkPos::of_block(target).region()) {
        owner.internal(RegionInternal::Run(RegionTask::new(move |there| {
            if there.owns(target)
                && there
                    .shared()
                    .growth_behaviors
                    .clone()
                    .rains_at(&*there, target)
            {
                strike_lightning(there, target);
            }
        })));
    }
}

/// Living things over `center` within lightning's targeting box that can
/// see the sky, as blocks vanilla's living-entity target search.
fn sky_visible_targets(world: &dyn RegionWorld, center: BlockPos) -> Vec<BlockPos> {
    let has_skylight = world.dimension_info().kind.has_skylight;
    let can_see_sky =
        |pos: BlockPos| has_skylight && world.light(pos).is_some_and(|(sky, _)| sky >= 15);
    let near = |x: i32, z: i32| {
        (x - center.x).abs() <= TARGET_RADIUS && (z - center.z).abs() <= TARGET_RADIUS
    };
    let block_of =
        |[x, y, z]: [f64; 3]| BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
    let mut found = Vec::new();
    for player in world.present_players() {
        let block = block_of(player.position);
        if near(block.x, block.z) && can_see_sky(block) {
            found.push(block);
        }
    }
    for id in world.entity_ids() {
        let Some(tracked) = world.entity(id) else {
            continue;
        };
        if !tracked.entity.living().is_some_and(|l| l.alive) {
            continue;
        }
        let block = block_of(tracked.entity.base().position);
        if near(block.x, block.z) && can_see_sky(block) {
            found.push(block);
        }
    }
    found
}

fn strike_lightning(world: &mut dyn RegionWorld, target: BlockPos) {
    let hooks = world.shared().region_hooks.clone();
    let trap = hooks
        .iter()
        .any(|hook| hook.natural_lightning(world, target));
    let id = world.shared().next_entity_id();
    let uuid = world.rng().r#gen();
    let bolt = if trap {
        LightningBolt::visual(id, uuid, target)
    } else {
        LightningBolt::new(id, uuid, target)
    };
    world.add_entity(Box::new(bolt));
}
