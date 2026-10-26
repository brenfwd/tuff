//! What a dispenser does with items that act on the block or player in
//! front of it: buckets, bottles, bone meal, flint and steel, honeycomb,
//! glowstone, shears, shulker boxes and wearable items.

use rand::Rng;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::item::Bucket;
use tuff_vanilla_data::registry::{ParticleType, Potion, SoundEvent};
use tuff_vanilla_data::{Block, BlockState, Item, tag};

use super::interact::{Interact, Rest};
use crate::world::interaction;
use tuff_core::protocol::play;
use tuff_core::world::block_ledger::{ClaimReason, Remains, TntOrigin};
use tuff_core::world::game_event::{Context as GameContext, GameEvent};
use tuff_core::world::growth::{BONEMEAL_EVENT, GrowthLevel};
use tuff_core::world::region_api::{DispenseOutcome, Fired, RegionWorld};
use tuff_core::world::{framed, packets};
use tuff_item::item::{ItemStack, MintReason, PotionContents};
use tuff_vanilla_common::placement;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;
use tuff_world_model::pos::ChunkPos;

const AIR: BlockState = Block::Air.default_state();
const MAX_ANCHOR_CHARGES: u8 = 4;
const MUD_SPLASHES: usize = 5;

/// A dispenser at `pos` facing `facing` fires `stack`; `None` when this is
/// not an item it knows.
pub fn dispense(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    facing: Facing,
    stack: &Fired,
) -> Option<DispenseOutcome> {
    let target = placement::step(pos, facing);
    let item = stack.item();
    if matches!(item, Item::CarvedPumpkin | Item::WitherSkeletonSkull) {
        return None;
    }
    if !acts_on_target(item, stack) {
        return swallow(world, target, stack);
    }
    if !world.owns_chunk(ChunkPos::of_block(target)) {
        return Some(DispenseOutcome::Remote);
    }
    if matches!(item, Item::Shears | Item::GlassBottle)
        && world.block(target).is_some_and(full_hive)
    {
        return None;
    }
    let outcome = match item {
        Item::Bucket => Some(match world.take_fluid(target) {
            #[expect(
                clippy::disallowed_methods,
                reason = "item turning into another (dispense)"
            )]
            Some((filled, _)) => {
                world.game_event(
                    GameEvent::FluidPickup,
                    target.center(),
                    GameContext::default(),
                );
                DispenseOutcome::Exchanged(ItemStack::mint(filled, 1, MintReason::Transform))
            }
            None => DispenseOutcome::Dropped,
        }),
        Item::BoneMeal => Some(bone_meal(world, target, facing)),
        Item::GlassBottle => Some(fill_bottle(world, target)),
        Item::Potion => Some(water_to_mud(world, pos, target, stack)),
        Item::Honeycomb => Some(wax(world, target)),
        Item::Glowstone => Some(charge_anchor(world, target)),
        Item::FlintAndSteel => Some(ignite(world, target, facing, stack)),
        Item::Shears => Some(shear(world, target)),
        Item::Brush => Some(
            world
                .dispense_on_entity_in(target, stack)
                .unwrap_or(DispenseOutcome::Failed),
        ),
        Item::Chest => Some(
            world
                .dispense_on_entity_in(target, stack)
                .unwrap_or(DispenseOutcome::Dropped),
        ),
        _ if item.block().is_some_and(is_shulker_box) => {
            Some(place_shulker_box(world, target, facing, stack))
        }
        _ => match item.bucket() {
            Some(Bucket::Fluid {
                fluid: Some(fluid),
                mob,
            }) => Some(pour(world, target, fluid, mob, stack)),
            Some(Bucket::Solid { block, place_sound }) => {
                Some(pour_solid(world, target, block, place_sound))
            }
            #[expect(
                clippy::disallowed_methods,
                reason = "dispenser fills a mob bucket from a bucket of fluid (dispense)"
            )]
            Some(Bucket::Fluid {
                fluid: None,
                mob: Some(kind),
            }) => {
                let data = stack
                    .components
                    .other
                    .get_compound("minecraft:bucket_entity_data")
                    .cloned();
                world.release_bucket_mob(kind, target, data, Some(&stack.components.other), None);
                Some(DispenseOutcome::Exchanged(ItemStack::mint(
                    Item::Bucket,
                    1,
                    MintReason::Transform,
                )))
            }
            _ => None,
        },
    };
    outcome.or_else(|| {
        if stack.equippable().is_some_and(|e| e.dispensable) {
            Some(
                world
                    .dispense_equip_in(target, stack)
                    .unwrap_or(DispenseOutcome::Dropped),
            )
        } else {
            swallow(world, target, stack)
        }
    })
}

fn swallow(
    world: &mut dyn RegionWorld,
    target: BlockPos,
    stack: &Fired,
) -> Option<DispenseOutcome> {
    if !tuff_datapack::tags::is(tag::item::key::SULFUR_CUBE_SWALLOWABLE, stack.item()) {
        return None;
    }
    world.dispense_on_entity_in(target, stack)
}

fn acts_on_target<M: tuff_item::item::Mode>(item: Item, stack: &ItemStack<M>) -> bool {
    matches!(
        item,
        Item::Bucket
            | Item::BoneMeal
            | Item::GlassBottle
            | Item::Potion
            | Item::Honeycomb
            | Item::Glowstone
            | Item::FlintAndSteel
            | Item::Shears
            | Item::Brush
            | Item::Chest
    ) || item.block().is_some_and(is_shulker_box)
        || item.bucket().is_some()
        || stack.equippable().is_some_and(|e| e.dispensable)
}

/// A hive full of honey: what a dispenser's shears or bottle does to one
/// is the bees' business.
fn full_hive(state: BlockState) -> bool {
    matches!(state.block(), Block::Beehive | Block::BeeNest)
        && state.get("honey_level") == Some("5")
}

fn shear(world: &mut dyn RegionWorld, target: BlockPos) -> DispenseOutcome {
    if world.shear_entity_in(target) {
        DispenseOutcome::Worn(1)
    } else {
        DispenseOutcome::Failed
    }
}

fn is_shulker_box(block: Block) -> bool {
    tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::SHULKER_BOXES, block)
}

#[expect(
    clippy::disallowed_methods,
    reason = "dispenser pours a fluid and leaves an empty bucket (pour)"
)]
fn pour<M: tuff_item::item::Mode>(
    world: &mut dyn RegionWorld,
    target: BlockPos,
    fluid: Block,
    mob: Option<tuff_vanilla_data::registry::EntityType>,
    stack: &ItemStack<M>,
) -> DispenseOutcome {
    if !world.owns(target) || !world.dimension_info().contains_block(target) {
        return DispenseOutcome::Dropped;
    }
    let evaporates = world.dimension_info().environment().water_evaporates;
    let found = {
        let level = |p: BlockPos| world.block(p).unwrap_or(AIR);
        interaction::dispense_pour(target, fluid, evaporates, &level)
    };
    let Some((at, pour)) = found else {
        return DispenseOutcome::Dropped;
    };
    let mob_data = mob.and_then(|_| {
        stack
            .components
            .other
            .get_compound("minecraft:bucket_entity_data")
            .cloned()
    });
    world.pour_fluid(at, fluid, pour, mob, mob_data, None);
    world.game_event(GameEvent::FluidPlace, at.center(), GameContext::default());
    DispenseOutcome::Exchanged(ItemStack::mint(Item::Bucket, 1, MintReason::Transform))
}

#[expect(
    clippy::disallowed_methods,
    reason = "dispenser pours a solid bucket and leaves an empty bucket (pour_solid)"
)]
fn pour_solid(
    world: &mut dyn RegionWorld,
    target: BlockPos,
    block: Block,
    sound: SoundEvent,
) -> DispenseOutcome {
    if !world.owns(target)
        || !world.dimension_info().contains_block(target)
        || !world.block(target).is_some_and(is_air)
    {
        return DispenseOutcome::Dropped;
    }
    world.put_block(target, block.default_state());
    world.play_sound(sound, target, 1.0, 1.0, None);
    world.game_event(
        GameEvent::FluidPlace,
        target.center(),
        GameContext::default(),
    );
    DispenseOutcome::Exchanged(ItemStack::mint(Item::Bucket, 1, MintReason::Transform))
}

fn bone_meal(world: &mut dyn RegionWorld, target: BlockPos, facing: Facing) -> DispenseOutcome {
    let behaviours = world.shared().growth_behaviors.clone();
    let level: &mut dyn GrowthLevel = world;
    match behaviours.bonemeal(level, target, facing.opposite()) {
        Some(grown) => {
            world.broadcast_level_event(BONEMEAL_EVENT, grown, 15);
            DispenseOutcome::Dispensed
        }
        None => DispenseOutcome::Failed,
    }
}

fn is_water(state: BlockState) -> bool {
    tuff_datapack::tags::is(
        tuff_vanilla_data::tag::fluid::key::WATER,
        state.fluid_state().fluid(),
    )
}

fn water_bottle() -> ItemStack {
    #[expect(
        clippy::disallowed_methods,
        reason = "item turning into another (water_bottle)"
    )]
    let mut stack = ItemStack::mint(Item::Potion, 1, MintReason::Transform);
    stack.components.potion_contents = Some(Box::new(PotionContents {
        potion: Some(Potion::Water),
        ..PotionContents::default()
    }));
    stack
}

fn fill_bottle(world: &mut dyn RegionWorld, target: BlockPos) -> DispenseOutcome {
    if world.block(target).is_some_and(is_water) {
        DispenseOutcome::Exchanged(water_bottle())
    } else {
        DispenseOutcome::DroppedFailing
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "dispenser turns water into mud and leaves a bottle (water_to_mud)"
)]
fn water_to_mud<M: tuff_item::item::Mode>(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    target: BlockPos,
    stack: &ItemStack<M>,
) -> DispenseOutcome {
    let water = stack
        .components
        .potion_contents
        .as_ref()
        .is_some_and(|c| c.potion == Some(Potion::Water));
    let convertible = world.block(target).is_some_and(|s| {
        tuff_datapack::tags::is(
            tuff_vanilla_data::tag::block::key::CONVERTIBLE_TO_MUD,
            s.block(),
        )
    });
    if !water || !convertible || !world.owns(target) {
        return DispenseOutcome::Dropped;
    }
    let mut splashes = Vec::with_capacity(MUD_SPLASHES);
    for _ in 0..MUD_SPLASHES {
        let x = f64::from(pos.x) + world.rng().r#gen::<f64>();
        let z = f64::from(pos.z) + world.rng().r#gen::<f64>();
        splashes.push(framed(&play::Particles {
            particle: ParticleType::Splash.id(),
            position: [x, f64::from(pos.y + 1), z],
            spread: [0.0; 3],
            speed: 1.0,
            count: 1,
            alternative: false,
        }));
    }
    world.send_to_chunk(ChunkPos::of_block(pos), None, packets(splashes));
    world.play_sound(SoundEvent::ItemBottleEmpty, pos, 1.0, 1.0, None);
    world.put_block(target, Block::Mud.default_state());
    DispenseOutcome::Exchanged(ItemStack::mint(Item::GlassBottle, 1, MintReason::Transform))
}

fn wax(world: &mut dyn RegionWorld, target: BlockPos) -> DispenseOutcome {
    let Some(waxed) = world.block(target).and_then(interaction::waxed) else {
        return DispenseOutcome::Dropped;
    };
    world.put_block(target, waxed);
    world.broadcast_level_event(play::level_event::PARTICLES_WAX_ON, target, 0);
    world.play_sound(SoundEvent::ItemHoneycombWaxOn, target, 1.0, 1.0, None);
    DispenseOutcome::Dispensed
}

fn charge_anchor(world: &mut dyn RegionWorld, target: BlockPos) -> DispenseOutcome {
    let Some(state) = world.block(target) else {
        return DispenseOutcome::Dropped;
    };
    if state.block() != Block::RespawnAnchor {
        return DispenseOutcome::Dropped;
    }
    if state.charges().is_none_or(|c| c >= MAX_ANCHOR_CHARGES) {
        return DispenseOutcome::Failed;
    }
    world.charge_anchor(target, state);
    DispenseOutcome::Dispensed
}

/// Flint and steel: primes an explosive sulfur cube in front, else fire
/// in the air, else lights a campfire or candle, else primes TNT. It wears
/// even when there was nothing to light.
fn ignite(
    world: &mut dyn RegionWorld,
    target: BlockPos,
    facing: Facing,
    stack: &Fired,
) -> DispenseOutcome {
    if let Some(outcome) = world.dispense_on_entity_in(target, stack) {
        return outcome;
    }
    let Some(state) = world.block(target) else {
        return DispenseOutcome::Worn(1);
    };
    if is_air(state) {
        let hooks = world.shared().region_hooks.clone();
        if hooks
            .iter()
            .any(|hook| hook.light_portal(world, target, Some(facing)))
        {
            return DispenseOutcome::Worn(1);
        }
        let fire = {
            let level = |p: BlockPos| world.block(p).unwrap_or(AIR);
            let fire = placement::fire_state(target, &level);
            placement::can_survive(fire, target, &level).then_some(fire)
        };
        if let Some(fire) = fire {
            world.put_block(target, fire);
        }
    } else if interaction::can_light(state) {
        if let Some(lit) = state.with_lit(true) {
            world.put_block(target, lit);
        }
    } else if state.block() == Block::Tnt {
        if !world.shared().rule_bool("tnt_explodes") {
            return DispenseOutcome::Failed;
        }
        let Some(broken) = world.claim_block(target, Remains::Air, ClaimReason::Entity) else {
            return DispenseOutcome::Failed;
        };
        world.spawn_primed_tnt(TntOrigin::Block(broken), target, None, None);
    }
    DispenseOutcome::Worn(1)
}

fn place_shulker_box(
    world: &mut dyn RegionWorld,
    target: BlockPos,
    facing: Facing,
    stack: &tuff_item::item::ItemPicture,
) -> DispenseOutcome {
    let Some(block) = stack.item().block() else {
        return DispenseOutcome::Failed;
    };
    let below = placement::step(target, Facing::Down);
    let face = if world.block(below).is_some_and(is_air) {
        facing
    } else {
        Facing::Up
    };
    let replaceable = world.block(target).is_some_and(|s| s.is_replaceable());
    let state = block.default_state().with_facing(face);
    match state {
        #[expect(
            clippy::disallowed_methods,
            reason = "dispenser places a shulker box through the item placement door (place_shulker_box)"
        )]
        Some(state) if replaceable && world.place_item_block(target, state, stack) => {
            DispenseOutcome::Dispensed
        }
        _ => DispenseOutcome::Failed,
    }
}
