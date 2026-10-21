//! Golems and the wither built from blocks: a placed head that completes
//! a body turns it into the mob.

use rand::Rng;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Block, BlockState, Item};

use crate::{golem, wither};
use tuff_core::world::messages::{AdvancementEvent, PlayerEvent};
use tuff_core::world::player::Difficulty;
use tuff_core::world::predicate::EntityFacts;
use tuff_core::world::region_api::{DispenseOutcome, RegionWorld};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;
use tuff_world_model::pos::ChunkPos;

const AIR: BlockState = Block::Air.default_state();

/// A player placed `state` at `pos`: builds whatever it completes.
pub fn placed(world: &mut dyn RegionWorld, pos: BlockPos, state: BlockState) {
    if matches!(state.block(), Block::CarvedPumpkin | Block::JackOLantern)
        && let Some(assembly) = golem::try_assemble(pos, |p| world.block(p))
    {
        assemble_golem(world, assembly);
    }
    if matches!(
        state.block(),
        Block::WitherSkeletonSkull | Block::WitherSkeletonWallSkull
    ) && world.shared().difficulty() != Difficulty::Peaceful
        && let Some(assembly) = wither::try_assemble(pos, |p| world.block(p))
    {
        assemble_wither(world, assembly);
    }
}

/// A dispenser facing `facing` fires a carved pumpkin or wither skeleton
/// skull at `target`: placed there when it completes a golem or the
/// wither, else worn by whoever stands there.
pub fn dispensed(
    world: &mut dyn RegionWorld,
    target: BlockPos,
    facing: Facing,
    stack: &tuff_core::world::region_api::Fired,
) -> Option<DispenseOutcome> {
    let head = match stack.item {
        Item::CarvedPumpkin => Some(Block::CarvedPumpkin.default_state()),
        Item::WitherSkeletonSkull => Block::WitherSkeletonSkull
            .default_state()
            .with_rotation(skull_segment(facing))
            .filter(|_| {
                world.shared().difficulty() != Difficulty::Peaceful
                    && target.y >= world.dimension_info().shape.min_y + 2
            }),
        _ => return None,
    };
    if !world.owns_chunk(ChunkPos::of_block(target)) {
        return Some(DispenseOutcome::Remote);
    }
    if let Some(head) = head.filter(|_| world.block(target).is_some_and(is_air)) {
        let lookup = |p| {
            if p == target {
                Some(head)
            } else {
                world.block(p)
            }
        };
        let completes = if stack.item == Item::CarvedPumpkin {
            golem::try_assemble(target, lookup).is_some()
        } else {
            wither::try_assemble(target, lookup).is_some()
        };
        if completes {
            world.put_block(target, head);
            placed(world, target, head);
            return Some(DispenseOutcome::Dispensed);
        }
    }
    Some(
        world
            .dispense_equip_in(target, stack)
            .unwrap_or(DispenseOutcome::Failed),
    )
}

fn skull_segment(facing: Facing) -> u8 {
    match facing {
        Facing::West => 4,
        Facing::North => 8,
        Facing::East => 12,
        _ => 0,
    }
}

/// Turns a completed golem body into the mob: clears the blocks it used
/// and spawns it in their place.
fn assemble_golem(world: &mut dyn RegionWorld, assembly: golem::Assembly) {
    for pos in &assembly.consumed {
        world.put_block(*pos, AIR);
    }
    if let Some((pos, state)) = assembly.replace {
        world.put_block(pos, state);
    }
    let id = world.shared().next_entity_id();
    let yaw = world.rng().r#gen::<f32>() * 360.0;
    let Some(mut entity) =
        world
            .shared()
            .entity_types
            .create(assembly.kind, id, assembly.spawn, yaw)
    else {
        return;
    };
    if assembly.kind == EntityType::IronGolem {
        golem::mark_player_created(entity.as_mut());
    }
    if assembly.kind == EntityType::CopperGolem {
        golem::mark_copper_oxidation(entity.as_mut(), assembly.oxidation);
        world.play_sound(
            SoundEvent::EntityCopperGolemSpawn,
            assembly.consumed[0],
            1.0,
            1.0,
            None,
        );
    }
    world.add_entity(entity);
    notify_summoned(world, assembly.kind, assembly.spawn, 5.0);
}

/// Tells players within `range` of `spawn` that `kind` was summoned there.
fn notify_summoned(world: &mut dyn RegionWorld, kind: EntityType, spawn: [f64; 3], range: f64) {
    let facts = EntityFacts {
        kind: Some(kind),
        position: spawn,
        dimension: world.dimension_info().key.clone(),
        ..Default::default()
    };
    for (handle, position) in world.seen_players() {
        if tuff_core::world::distance_sq(position, spawn) <= range * range {
            handle.send(PlayerEvent::Advancement(Box::new(
                AdvancementEvent::SummonedEntity(facts.clone()),
            )));
        }
    }
}

/// Turns a completed wither body into the boss: clears the blocks it
/// used, spawns it invulnerable (its birth animation), and tells nearby
/// players' "summon a wither" advancement criteria about it.
fn assemble_wither(world: &mut dyn RegionWorld, assembly: wither::Assembly) {
    for pos in &assembly.consumed {
        world.put_block(*pos, AIR);
    }
    let id = world.shared().next_entity_id();
    let Some(mut entity) =
        world
            .shared()
            .entity_types
            .create(EntityType::Wither, id, assembly.spawn, assembly.yaw)
    else {
        return;
    };
    wither::make_invulnerable(entity.as_mut());
    world.add_entity(entity);
    notify_summoned(world, EntityType::Wither, assembly.spawn, 50.0);
}
