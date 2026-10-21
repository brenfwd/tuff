//! The skeleton trap: a skeleton horse a thunderstorm leaves behind which,
//! once a player comes within 10 blocks, calls a harmless lightning flash
//! and becomes four tamed skeleton horses ridden by helmeted skeletons.

use rand::Rng;
use tuff_item::item::DupUnchecked;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::EntityType;

use super::equine::SkeletonHorse;
use super::goal::{Flags, Goal};
use super::monster::LocalDifficulty;
use super::skeleton::Skeleton;
use super::{Mob, MobCore};
use tuff_core::protocol::play;
use tuff_core::world::entity::{Effect, RegionEntity, TickContext};
use tuff_core::world::region_api::RegionWorld;
use tuff_core::world::{framed, packets};
use tuff_item::item::{ItemStack, MintReason};
use tuff_world_model::BlockPos;

const TRIGGER_RANGE: f64 = 10.0;
const HORSEMEN: usize = 3;

/// The trap's goal, running while `MobCore::signal` marks a trap horse.
#[derive(Debug)]
pub struct SkeletonTrap;

impl Goal for SkeletonTrap {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.signal != 0
            && cx
                .players
                .iter()
                .any(|player| mob.distance_sq(player.position) <= TRIGGER_RANGE * TRIGGER_RANGE)
    }

    fn can_continue(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        false
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        spring(mob, cx);
    }
}

/// Springs the trap on `horse`: it is tamed and grown, and three more
/// horses join it, each with a skeleton rider.
pub fn spring(horse: &mut MobCore, cx: &mut TickContext) {
    horse.signal = 0;
    tame(horse);
    let position = horse.base.position;
    flash(position, cx);
    let difficulty = LocalDifficulty::at(cx, position);
    if let Some(rider) = skeleton(position, difficulty, cx) {
        let passenger = rider.base().id;
        cx.effects.push(Effect::Spawn(rider));
        cx.effects.push(Effect::Mount {
            passenger,
            vehicle: horse.base.id,
        });
    }
    for _ in 0..HORSEMEN {
        let Some(mut other) = trap_horse(position, cx) else {
            continue;
        };
        let Some(rider) = skeleton(position, difficulty, cx) else {
            continue;
        };
        let push = [triangle(cx), 0.0, triangle(cx)];
        let base = other.base_mut();
        base.passengers
            .push(tuff_core::world::entity::save(&*rider));
        for axis in 0..3 {
            base.velocity[axis] += push[axis];
        }
        cx.effects.push(Effect::Spawn(other));
    }
}

/// A fresh untamed trap horse, as a thunderstorm leaves one: not yet
/// sprung, it vanishes after 18000 ticks unless something keeps it.
pub fn trap(
    shared: &tuff_core::world::Shared,
    id: i32,
    position: [f64; 3],
) -> Option<Box<dyn RegionEntity>> {
    let mut horse = shared
        .entity_types
        .create(EntityType::SkeletonHorse, id, position, 0.0)?;
    let mob = horse.downcast_mut::<Mob<SkeletonHorse>>()?;
    mob.core.signal = 1;
    mob.core.set_age(0);
    Some(horse)
}

/// A thunderstorm strikes `target`: true when the strike leaves a trap
/// horse there instead of striking for real.
pub fn natural_strike(world: &mut dyn RegionWorld, target: BlockPos) -> bool {
    if !springs_trap(world, target) {
        return false;
    }
    let id = world.shared().next_entity_id();
    let position = [target.x as f64, target.y as f64, target.z as f64];
    if let Some(horse) = trap(&world.shared().clone(), id, position) {
        world.add_entity(horse);
    }
    true
}

fn springs_trap(world: &mut dyn RegionWorld, target: BlockPos) -> bool {
    let shared = world.shared().clone();
    if !shared.game_rules.borrow().bool("spawn_mobs") {
        return false;
    }
    let position = [target.x as f64, target.y as f64, target.z as f64];
    let effective = LocalDifficulty::in_region(world, position).effective;
    world.rng().r#gen::<f64>() < f64::from(effective) * 0.01
        && !world
            .block(BlockPos::new(target.x, target.y - 1, target.z))
            .is_some_and(|state| {
                tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::block::key::LIGHTNING_RODS,
                    state.block(),
                )
            })
}

fn triangle(cx: &mut TickContext) -> f64 {
    1.1485 * (cx.rng.r#gen::<f64>() - cx.rng.r#gen::<f64>())
}

fn tame(core: &mut MobCore) {
    if let Some(tame) = &mut core.tame {
        tame.ownerless = true;
    }
    core.set_age(0);
    core.changed(super::equine::FLAGS);
}

/// A lightning bolt only clients see: the flash and thunder, no strike.
fn flash(position: [f64; 3], cx: &mut TickContext) {
    let packet = framed(&play::AddEntity {
        entity_id: cx.shared.next_entity_id(),
        uuid: cx.rng.r#gen(),
        kind: EntityType::LightningBolt.id() as i32,
        position,
        velocity: [0.0; 3],
        pitch: 0.0,
        yaw: 0.0,
        head_yaw: 0.0,
        data: 0,
    });
    cx.effects.push(Effect::ChunkPackets(packets([packet])));
}

fn trap_horse(position: [f64; 3], cx: &mut TickContext) -> Option<Box<dyn RegionEntity>> {
    let id = cx.shared.next_entity_id();
    let mut horse = cx
        .shared
        .entity_types
        .create(EntityType::SkeletonHorse, id, position, 0.0)?;
    horse.finalize_spawn(cx, 0);
    let mob = horse.downcast_mut::<Mob<SkeletonHorse>>()?;
    mob.core.persistence_required = true;
    tame(&mut mob.core);
    Some(horse)
}

fn skeleton(
    position: [f64; 3],
    difficulty: LocalDifficulty,
    cx: &mut TickContext,
) -> Option<Box<dyn RegionEntity>> {
    let id = cx.shared.next_entity_id();
    let mut skeleton = cx
        .shared
        .entity_types
        .create(EntityType::Skeleton, id, position, 0.0)?;
    skeleton.finalize_spawn(cx, 0);
    let mob = skeleton.downcast_mut::<Mob<Skeleton>>()?;
    mob.core.persistence_required = true;
    if mob.core.equipment.get(EquipmentSlot::Head).is_none() {
        mob.core.equipment.set(
            EquipmentSlot::Head,
            Some(ItemStack::mint(Item::IronHelmet, 1, MintReason::Spawn)),
        );
    }
    let special = difficulty.special_multiplier();
    for slot in [EquipmentSlot::Mainhand, EquipmentSlot::Head] {
        let Some(mut stack) = mob
            .core
            .equipment
            .get(slot)
            .map(DupUnchecked::dup_unchecked)
        else {
            continue;
        };
        stack.components.enchantments = None;
        let stack = tuff_item::item::enchanting::enchant_from_provider(
            stack,
            "minecraft:mob_spawn_equipment",
            special,
            cx.rng,
        );
        mob.core.equipment.set(slot, Some(stack));
    }
    Some(skeleton)
}
