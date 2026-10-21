//! Blocks with a small random tick of their own: redstone ore going dark,
//! eyeblossoms following the time of day, dried ghasts soaking, and nether
//! portals spawning zombified piglins.

use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Block, BlockState};

use super::{Behaviours, GrowthLevel, GrowthLevelExt, above, below, offset};
use tuff_core::world::redstone::{UPDATE_ALL, UPDATE_CLIENTS};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

const EYEBLOSSOM_OPEN_COLOR: i32 = 16_545_810;
const EYEBLOSSOM_CLOSED_COLOR: i32 = 6_250_335;
const EYEBLOSSOM_XZ_RANGE: i32 = 3;
const EYEBLOSSOM_Y_RANGE: i32 = 2;
const HYDRATION_DELAY: u32 = 5000;
const SPAWN_ROLL: i32 = 2000;
const PORTAL_COOLDOWN: i32 = 300;

pub fn register(behaviours: &mut Behaviours) {
    behaviours.register_random_kind("redstone_ore", redstone_ore_tick);
    behaviours.register_random_kind("eyeblossom", eyeblossom_random_tick);
    behaviours.register_random_kind("dried_ghast", dried_ghast_random_tick);
    behaviours.register_random_kind("nether_portal", portal_tick);
    for block in [Block::OpenEyeblossom, Block::ClosedEyeblossom] {
        behaviours.register_scheduled(block, eyeblossom_scheduled_tick);
    }
    behaviours.register_scheduled(Block::DriedGhast, dried_ghast_tick);
}

fn redstone_ore_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if state.lit() == Some(true)
        && let Some(dark) = state.with_lit(false)
    {
        level.set(pos, dark, UPDATE_ALL);
    }
}

fn try_changing_eyeblossom(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) -> bool {
    let open = state.block() == Block::OpenEyeblossom;
    let environment = level.environment();
    let should_open = environment
        .eyeblossom_open(|clock| level.clock_ticks(clock))
        .unwrap_or(open);
    if should_open == open {
        return false;
    }
    let next = if open {
        Block::ClosedEyeblossom
    } else {
        Block::OpenEyeblossom
    };
    level.set(pos, next.default_state(), UPDATE_ALL);
    let lifetime = 0.5 + level.random_double();
    let velocity = [
        level.random_double() - 0.5,
        level.random_double() + 1.0,
        level.random_double() - 0.5,
    ];
    let start = [
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
    ];
    let target = [0, 1, 2].map(|i| start[i] + velocity[i] * lifetime);
    let color = if open {
        EYEBLOSSOM_CLOSED_COLOR
    } else {
        EYEBLOSSOM_OPEN_COLOR
    };
    level.trail_particle(start, target, color, (20.0 * lifetime) as i32);
    let (min, max) = (
        offset(
            pos,
            -EYEBLOSSOM_XZ_RANGE,
            -EYEBLOSSOM_Y_RANGE,
            -EYEBLOSSOM_XZ_RANGE,
        ),
        offset(
            pos,
            EYEBLOSSOM_XZ_RANGE,
            EYEBLOSSOM_Y_RANGE,
            EYEBLOSSOM_XZ_RANGE,
        ),
    );
    for x in min.x..=max.x {
        for y in min.y..=max.y {
            for z in min.z..=max.z {
                let near = BlockPos::new(x, y, z);
                if level.block(near) != Some(state) {
                    continue;
                }
                let distance =
                    (((pos.x - x).pow(2) + (pos.y - y).pow(2) + (pos.z - z).pow(2)) as f64).sqrt();
                let (low, high) = ((distance * 5.0) as i32, (distance * 10.0) as i32);
                let delay = low + level.random_int(high - low + 1);
                level.schedule_tick(near, state.block(), delay.max(0) as u32);
            }
        }
    }
    true
}

fn eyeblossom_random_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if try_changing_eyeblossom(level, pos, state) {
        let sound = if state.block() == Block::OpenEyeblossom {
            SoundEvent::BlockEyeblossomCloseLong
        } else {
            SoundEvent::BlockEyeblossomOpenLong
        };
        level.play_block_sound(sound, pos, 1.0, 1.0);
    }
}

fn eyeblossom_scheduled_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    if try_changing_eyeblossom(level, pos, state) {
        let sound = if state.block() == Block::OpenEyeblossom {
            SoundEvent::BlockEyeblossomClose
        } else {
            SoundEvent::BlockEyeblossomOpen
        };
        level.play_block_sound(sound, pos, 1.0, 1.0);
    }
}

fn dried_ghast_random_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let wet = state.waterlogged() == Some(true) || state.hydration().unwrap_or(0) > 0;
    if wet && !level.has_scheduled_tick(pos, Block::DriedGhast) {
        level.schedule_tick(pos, Block::DriedGhast, HYDRATION_DELAY);
    }
}

fn dried_ghast_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let hydration = state.hydration().unwrap_or(0);
    if state.waterlogged() == Some(true) {
        if hydration < 3 {
            level.play_block_sound(SoundEvent::BlockDriedGhastTransition, pos, 1.0, 1.0);
            if let Some(wetter) = state.with_hydration(hydration + 1) {
                level.set(pos, wetter, UPDATE_CLIENTS);
            }
        } else {
            spawn_ghastling(level, pos, state);
        }
    } else if hydration > 0
        && let Some(drier) = state.with_hydration(hydration - 1)
    {
        level.set(pos, drier, UPDATE_CLIENTS);
    }
}

fn spawn_ghastling(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    level.destroy(pos, false);
    let yaw = match state.facing() {
        Some(tuff_vanilla_data::block::Facing::North) => 180.0,
        Some(tuff_vanilla_data::block::Facing::East) => 270.0,
        Some(tuff_vanilla_data::block::Facing::West) => 90.0,
        _ => 0.0,
    };
    let nbt = Compound::new()
        .with("id", EntityType::HappyGhast.name())
        .with("Age", -24000)
        .with(
            "Rotation",
            Tag::List(vec![Tag::Float(yaw), Tag::Float(0.0)]),
        );
    let position = [pos.x as f64 + 0.5, pos.y as f64, pos.z as f64 + 0.5];
    if level.summon(nbt, position, false) {
        level.play_block_sound(SoundEvent::EntityGhastlingSpawn, pos, 1.0, 1.0);
    }
}

fn portal_tick(level: &mut dyn GrowthLevel, pos: BlockPos, _state: BlockState) {
    if level.difficulty() == 0
        || !level.rule_bool("spawn_monsters")
        || !level.environment().nether_portal_spawns_piglin
    {
        return;
    }
    let difficulty = level.difficulty() as i32;
    if level.random_int(SPAWN_ROLL) >= difficulty || !level.player_within(pos, 128) {
        return;
    }
    let mut ground = pos;
    while level.state(ground).block() == Block::NetherPortal {
        ground = below(ground);
    }
    if !level
        .state(ground)
        .is_valid_spawn(EntityType::ZombifiedPiglin)
    {
        return;
    }
    let nbt = Compound::new()
        .with("id", EntityType::ZombifiedPiglin.name())
        .with("PortalCooldown", PORTAL_COOLDOWN);
    let spawn = above(ground);
    let position = [spawn.x as f64 + 0.5, spawn.y as f64, spawn.z as f64 + 0.5];
    level.summon(nbt, position, true);
}

#[cfg(test)]
mod tests {
    use super::super::testing::TestLevel;
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn lit_redstone_ore_goes_dark() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        let lit = Block::RedstoneOre.default_state().with_lit(true).unwrap();
        level.put(pos, lit);
        level.run(redstone_ore_tick, pos);
        assert_eq!(level.state(pos).lit(), Some(false));
    }

    #[tuff_core::watchdog]
    #[test]
    fn eyeblossoms_open_at_night_and_wake_their_neighbours() {
        let mut level = TestLevel::new();
        let (a, b) = (BlockPos::new(0, 64, 0), BlockPos::new(2, 64, 0));
        for pos in [a, b] {
            level.put(pos, Block::ClosedEyeblossom.default_state());
        }
        level.clock = 6000;
        level.run(eyeblossom_random_tick, a);
        assert_eq!(level.state(a).block(), Block::ClosedEyeblossom);
        level.clock = 18000;
        level.run(eyeblossom_random_tick, a);
        assert_eq!(level.state(a).block(), Block::OpenEyeblossom);
        assert!(
            level
                .ticks
                .iter()
                .any(|(pos, block, _)| *pos == b && *block == Block::ClosedEyeblossom)
        );
        level.run(eyeblossom_scheduled_tick, b);
        assert_eq!(level.state(b).block(), Block::OpenEyeblossom);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_soaked_dried_ghast_hatches_a_baby_happy_ghast() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        let wet = Block::DriedGhast
            .default_state()
            .with_waterlogged(true)
            .unwrap();
        level.put(pos, wet);
        level.run(dried_ghast_random_tick, pos);
        assert_eq!(level.ticks.len(), 1);
        for expected in 1..=3 {
            level.run(dried_ghast_tick, pos);
            assert_eq!(level.state(pos).hydration(), Some(expected));
        }
        level.run(dried_ghast_tick, pos);
        assert_eq!(level.state(pos).block(), Block::Water);
        assert_eq!(level.spawned.len(), 1);
        assert_eq!(
            level.spawned[0].0.get_str("id"),
            Some("minecraft:happy_ghast")
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_dry_dried_ghast_loses_hydration() {
        let mut level = TestLevel::new();
        let pos = BlockPos::new(0, 64, 0);
        let damp = Block::DriedGhast.default_state().with_hydration(2).unwrap();
        level.put(pos, damp);
        level.run(dried_ghast_tick, pos);
        assert_eq!(level.state(pos).hydration(), Some(1));
    }

    #[tuff_core::watchdog]
    #[test]
    fn overworld_portals_spawn_zombified_piglins_near_players() {
        let mut level = TestLevel::new();
        level.player = Some(BlockPos::new(0, 64, 0));
        level.put(BlockPos::new(0, 63, 0), Block::Obsidian.default_state());
        let portal = BlockPos::new(0, 64, 0);
        level.put(portal, Block::NetherPortal.default_state());
        level.put(above(portal), Block::NetherPortal.default_state());
        for _ in 0..4000 {
            level.run(portal_tick, above(portal));
        }
        assert!(!level.spawned.is_empty());
        assert_eq!(level.spawned[0].1, [0.5, 64.0, 0.5]);
    }
    #[tuff_core::watchdog]
    #[test]
    fn a_closed_eyeblossom_opening_at_night_sends_one_orange_trail_particle() {
        let mut level = TestLevel::new();
        level.clock = 18_000;
        let pos = BlockPos::new(3, 64, 3);
        let closed = Block::ClosedEyeblossom.default_state();
        level.put(pos, closed);
        eyeblossom_scheduled_tick(&mut level, pos, closed);
        assert_eq!(
            level.block(pos).map(|s| s.block()),
            Some(Block::OpenEyeblossom)
        );
        assert_eq!(level.trails.len(), 1);
        let (from, target, color, duration) = level.trails[0];
        assert_eq!(from, [3.5, 64.5, 3.5]);
        assert_eq!(color, EYEBLOSSOM_OPEN_COLOR);
        assert!((10..=30).contains(&duration));
        assert!(target[1] > from[1]);
    }
}
