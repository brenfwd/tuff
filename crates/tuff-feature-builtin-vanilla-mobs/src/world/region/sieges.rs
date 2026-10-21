//! Village zombie sieges: at the roll marker of a night, one in ten
//! nights a player standing in a village gets twenty zombies, one every
//! three ticks, placed around a point 32 blocks off, within a village.
//!
//! The game keeps one siege per dimension. Each region keeps its own and
//! only considers the players standing here.

use rand::Rng;
use tuff_vanilla_data::entity::Heightmap;
use tuff_vanilla_data::registry::EntityType;

use crate::physics::block_pos;
use crate::raider;
use crate::spawning::{self, SpawnReason};
use tuff_core::world::clock::TimeMarker;
use tuff_core::world::player::Difficulty;
use tuff_core::world::region_api::RegionWorld;
use tuff_world_model::BlockPos;

const ROLL_MARKER: &str = "minecraft:roll_village_siege";
const SIEGE_ZOMBIES: i32 = 20;
const SPAWN_GAP: i32 = 2;
const RADIUS: f32 = 32.0;

#[derive(Default)]
struct Siege {
    marker: Option<Option<TimeMarker>>,
    tonight: bool,
    set_up: bool,
    zombies: i32,
    next_spawn: i32,
    at: BlockPos,
}

pub fn tick(world: &mut dyn RegionWorld) {
    let shared = world.shared().clone();
    let enemies = *shared.difficulty.borrow() != Difficulty::Peaceful
        && shared.rule_bool("spawn_mobs")
        && shared.rule_bool("spawn_monsters");
    let bright = !world.dimension_info().environment().has_fixed_time && world.sky_darken() < 4;
    if bright || !enemies {
        let siege = world.state().get::<Siege>();
        siege.tonight = false;
        siege.set_up = false;
        return;
    }
    let Some(clock) = world.dimension_info().default_clock else {
        return;
    };
    let marker = *world
        .state()
        .get::<Siege>()
        .marker
        .get_or_insert_with(|| TimeMarker::find(clock, ROLL_MARKER));
    let ticks = shared.clocks.borrow().get(clock).total_ticks;
    if marker.is_some_and(|m| m.occurs_at(ticks)) {
        let roll = world.rng().gen_range(0..10) == 0;
        world.state().get::<Siege>().tonight = roll;
    }
    if !world.state().get::<Siege>().tonight {
        return;
    }
    if !world.state().get::<Siege>().set_up {
        if !set_up(world) {
            return;
        }
        world.state().get::<Siege>().set_up = true;
    }
    let siege = world.state().get::<Siege>();
    if siege.next_spawn > 0 {
        siege.next_spawn -= 1;
        return;
    }
    siege.next_spawn = SPAWN_GAP;
    if siege.zombies > 0 {
        siege.zombies -= 1;
        let at = siege.at;
        try_spawn(world, at);
    } else {
        siege.tonight = false;
    }
}

fn set_up(world: &mut dyn RegionWorld) -> bool {
    for player in world.present_players() {
        let center = block_pos(player.position);
        let biome_ok = world.biome_name(center).is_none_or(|biome| {
            !tuff_datapack::tags::has(
                tuff_vanilla_data::tag::worldgen::biome::key::WITHOUT_ZOMBIE_SIEGES,
                biome,
            )
        });
        if !raider::is_village(world.pois(), center) || !biome_ok {
            continue;
        }
        for _ in 0..10 {
            let angle = world.rng().r#gen::<f32>() * std::f32::consts::TAU;
            let at = BlockPos::new(
                center.x + (angle.cos() * RADIUS).floor() as i32,
                center.y,
                center.z + (angle.sin() * RADIUS).floor() as i32,
            );
            if find_spawn_pos(world, at).is_some() {
                let siege = world.state().get::<Siege>();
                siege.at = at;
                siege.next_spawn = 0;
                siege.zombies = SIEGE_ZOMBIES;
                return true;
            }
        }
        return true;
    }
    false
}

fn find_spawn_pos(world: &mut dyn RegionWorld, around: BlockPos) -> Option<BlockPos> {
    let kind = EntityType::Zombie;
    let rule = world.shared().entity_types.spawn_rule(kind)?;
    let (min_y, top) = {
        let shape = &world.dimension_info().shape;
        (shape.min_y, shape.min_y + shape.height - 1)
    };
    for _ in 0..10 {
        let x = around.x + world.rng().gen_range(0..16) - 8;
        let z = around.z + world.rng().gen_range(0..16) - 8;
        let y = spawning::heightmap_top(Heightmap::WorldSurface, x, z, top, min_y, &|p| {
            world.block(p)
        });
        let pos = BlockPos::new(x, y, z);
        if !world.owns(pos) || !raider::is_village(world.pois(), pos) {
            continue;
        }
        let center = [f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5];
        if super::spawning::position_ok(world, kind, pos, center, rule, SpawnReason::Natural) {
            return Some(pos);
        }
    }
    None
}

fn try_spawn(world: &mut dyn RegionWorld, around: BlockPos) {
    let Some(pos) = find_spawn_pos(world, around) else {
        return;
    };
    let yaw = world.rng().r#gen::<f32>() * 360.0;
    let center = [
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    ];
    super::spawning::spawn_fresh(
        world,
        EntityType::Zombie,
        center,
        yaw,
        0,
        SpawnReason::Natural,
    );
}
