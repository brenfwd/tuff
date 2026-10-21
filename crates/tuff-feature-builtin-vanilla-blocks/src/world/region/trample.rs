//! Farmland trampled by something landing on it turns back to dirt, and
//! turtle eggs break under whatever steps or lands on them.

use rand::Rng;
use tuff_core::world::entity::TrackedEntity;
use tuff_core::world::messages::PlayerUpdate;
use tuff_core::world::redstone::{UPDATE_ALL, UPDATE_PUSH_ENTITIES};
use tuff_core::world::region_api::{Landing, RegionHooks, RegionWorld};
use tuff_vanilla_data::Block;
use tuff_vanilla_data::block::BlockKind;
use tuff_vanilla_data::registry::EntityType;
use tuff_world_model::BlockPos;

use crate::growth::turtle_egg;

/// The smallest `width^2 * height` that tramples farmland.
const MIN_TRAMPLE_VOLUME: f32 = 0.512;

pub struct Trample;

/// The block an entity standing at `position` stands on, as stepping
/// reactions see it.
fn stood_on(position: [f64; 3]) -> BlockPos {
    BlockPos::new(
        position[0].floor() as i32,
        (position[1] - 0.2).floor() as i32,
        position[2].floor() as i32,
    )
}

fn is_zombie(kind: EntityType) -> bool {
    matches!(
        kind,
        EntityType::Zombie
            | EntityType::Husk
            | EntityType::Drowned
            | EntityType::ZombieVillager
            | EntityType::ZombifiedPiglin
    )
}

fn protected(world: &dyn RegionWorld, pos: BlockPos, gamemaster: bool) -> bool {
    world
        .shared()
        .under_spawn_protection(world.dimension_id(), pos, gamemaster)
}

fn light_ore(world: &mut dyn RegionWorld, pos: BlockPos) {
    if let Some(state) = world.block(pos)
        && matches!(
            state.block(),
            Block::RedstoneOre | Block::DeepslateRedstoneOre
        )
        && state.lit() == Some(false)
        && let Some(lit) = state.with_lit(true)
    {
        world.set(pos, lit, UPDATE_ALL);
    }
}

/// A step lights redstone ore and breaks a turtle egg one in a hundred times.
fn step_on_blocks(
    world: &mut dyn RegionWorld,
    position: [f64; 3],
    kind: EntityType,
    living: bool,
    gamemaster: bool,
) {
    let pos = stood_on(position);
    light_ore(world, pos);
    if kind == EntityType::Player && protected(world, pos, gamemaster) {
        return;
    }
    if world
        .block(pos)
        .is_some_and(|s| s.block() == Block::TurtleEgg)
        && turtle_egg::breaks_eggs(world, kind, living)
    {
        turtle_egg::crush(world, pos, 100);
    }
}

impl RegionHooks for Trample {
    fn use_block(
        &self,
        world: &mut dyn RegionWorld,
        place: &tuff_core::world::messages::Place,
        _state: tuff_vanilla_data::BlockState,
    ) -> Option<tuff_core::world::messages::ActionOutcome> {
        light_ore(world, place.target);
        None
    }

    fn attack_block(
        &self,
        world: &mut dyn RegionWorld,
        pos: BlockPos,
        _state: tuff_vanilla_data::BlockState,
    ) {
        light_ore(world, pos);
    }

    fn player_moved(&self, world: &mut dyn RegionWorld, update: &PlayerUpdate) {
        if update.alive && update.grounded && !update.spectator && !update.looks.sneaking {
            step_on_blocks(
                world,
                update.position,
                EntityType::Player,
                true,
                update.gamemaster,
            );
        }
    }

    fn entity_moved(
        &self,
        world: &mut dyn RegionWorld,
        tracked: &mut TrackedEntity,
        _before: [f64; 3],
    ) {
        let base = tracked.entity.base();
        if base.on_ground {
            let (position, kind) = (base.position, base.kind);
            let living = tracked.entity.living().is_some_and(|l| l.alive);
            step_on_blocks(world, position, kind, living, false);
        }
    }

    fn entity_landed(&self, world: &mut dyn RegionWorld, landing: &Landing) {
        let Some(state) = world.block(landing.pos) else {
            return;
        };
        if state.block() == Block::HoneyBlock {
            super::honey::landed(world, landing);
            return;
        }
        if state.block() == Block::TurtleEgg {
            if !(landing.kind == EntityType::Player
                && protected(world, landing.pos, landing.gamemaster))
                && !is_zombie(landing.kind)
                && turtle_egg::breaks_eggs(world, landing.kind, landing.living)
            {
                turtle_egg::crush(world, landing.pos, 3);
            }
            return;
        }
        if state.block().kind() != BlockKind::Farmland {
            return;
        }
        let roll: f32 = world.rng().r#gen();
        if f64::from(roll) >= landing.fall_distance - 0.5 || !landing.living {
            return;
        }
        let allowed = if landing.kind == EntityType::Player {
            !protected(world, landing.pos, landing.gamemaster)
        } else {
            world.rule_bool("mob_griefing")
        };
        let [width, height] = landing.size.map(|v| v as f32);
        if allowed
            && width * width * height > MIN_TRAMPLE_VOLUME
            && let Some(base) = state.block().base_block()
        {
            world.set(
                landing.pos,
                base.default_state(),
                UPDATE_ALL | UPDATE_PUSH_ENTITIES,
            );
        }
    }
}
