//! Combat in regions: what a player's weapon does to an entity it hits,
//! Frost Walker freezing water under a player, and dispensers firing
//! projectiles, rockets, wind charges and TNT.

use tuff_vanilla_data::item::EquipmentSlot;

use crate::projectile::{
    arrow::Arrow,
    fireball::SmallFireball,
    firework::FireworkRocket,
    thrown::Thrown,
    wind_charge::{self, WindCharge},
};
use tuff_core::protocol::play::level_event;
use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::entity::{
    Culprit, Effect, EntityAttack, EntityHit, RegionEntity, TickContext,
};
use tuff_core::world::physics::triangle;
use tuff_core::world::projectile::{self, Pickup};
use tuff_core::world::region_api::DispenseOutcome;
use tuff_core::world::region_api::{RegionHooks, RegionWorld};
use tuff_item::item::ItemStack;
use tuff_vanilla_common::combat::{self, DamageContext, PostAttack, Side};
use tuff_vanilla_data::Item;
use tuff_vanilla_data::block::Facing;
use tuff_world_model::BlockPos;

const DISPENSED_POWER: f32 = 1.1;
const DISPENSED_UNCERTAINTY: f32 = 6.0;
const FIRE_CHARGE_SPREAD: f64 = 0.114_85;
/// A dispensed rocket starts just outside the dispenser's face.
const FIREWORK_OFFSET: f64 = 0.500_01;
const FIREWORK_POWER: f32 = 0.5;
const FIREWORK_UNCERTAINTY: f32 = 1.0;

/// Combat's region hooks: weapon effects on melee hits, Frost Walker and
/// dispensed arrows and TNT.
pub struct CombatRegionHooks;

impl RegionHooks for CombatRegionHooks {
    fn dispense(
        &self,
        world: &mut dyn RegionWorld,
        pos: BlockPos,
        facing: Facing,
        origin: [f64; 3],
        stack: &ItemStack,
    ) -> Option<DispenseOutcome> {
        let (sx, sy, sz) = facing.step();
        let step = [f64::from(sx), f64::from(sy), f64::from(sz)];
        match stack.item {
            Item::Arrow | Item::TippedArrow | Item::SpectralArrow => {
                let velocity = projectile::shot_velocity(
                    step,
                    DISPENSED_POWER,
                    DISPENSED_UNCERTAINTY,
                    world.rng(),
                );
                let id = world.shared().next_entity_id();
                let mut arrow = Arrow::from_item(
                    ItemStack::mint_like(stack, 1, tuff_item::item::MintReason::Transform),
                    id,
                    origin,
                    velocity,
                    projectile::Owner::default(),
                );
                arrow.pickup = Pickup::Allowed;
                world.add_entity(Box::new(arrow));
                Some(DispenseOutcome::Launched { sound: None })
            }
            Item::FireworkRocket => {
                let center = pos.center();
                let position = [0, 1, 2].map(|i| center[i] + step[i] * FIREWORK_OFFSET);
                let velocity = projectile::shot_velocity(
                    step,
                    FIREWORK_POWER,
                    FIREWORK_UNCERTAINTY,
                    world.rng(),
                );
                let id = world.shared().next_entity_id();
                let rocket = FireworkRocket::shot(
                    id,
                    position,
                    velocity,
                    ItemStack::mint_like(stack, 1, tuff_item::item::MintReason::Transform),
                    projectile::Owner::default(),
                    world.rng(),
                );
                world.add_entity(Box::new(rocket));
                Some(DispenseOutcome::Launched {
                    sound: Some(level_event::SOUND_FIREWORK_SHOOT),
                })
            }
            Item::WindCharge => {
                let center = pos.center();
                let position =
                    [0, 1, 2].map(|i| center[i] + step[i] * wind_charge::DISPENSE_OFFSET);
                let id = world.shared().next_entity_id();
                let charge = WindCharge::dispensed(id, position, step, world.rng());
                world.add_entity(Box::new(charge));
                Some(DispenseOutcome::Launched {
                    sound: Some(level_event::SOUND_WIND_CHARGE_SHOOT),
                })
            }
            item if projectile::thrown_kind(item).is_some() && item != Item::EnderPearl => {
                let kind = projectile::thrown_kind(item)?;
                let (power, uncertainty) = match item {
                    Item::SplashPotion | Item::LingeringPotion | Item::ExperienceBottle => {
                        (DISPENSED_POWER * 1.25, DISPENSED_UNCERTAINTY * 0.5)
                    }
                    _ => (DISPENSED_POWER, DISPENSED_UNCERTAINTY),
                };
                let velocity = projectile::shot_velocity(step, power, uncertainty, world.rng());
                let id = world.shared().next_entity_id();
                let thrown = Thrown::new(
                    kind,
                    id,
                    origin,
                    velocity,
                    ItemStack::mint_like(stack, 1, tuff_item::item::MintReason::Transform),
                    projectile::Owner::default(),
                );
                world.add_entity(Box::new(thrown));
                Some(DispenseOutcome::Launched { sound: None })
            }
            Item::FireCharge => {
                let center = pos.center();
                let position = [0, 1, 2].map(|i| center[i] + step[i]);
                let rng = world.rng();
                let direction = step.map(|c| triangle(rng, c, FIRE_CHARGE_SPREAD));
                let id = world.shared().next_entity_id();
                let fireball =
                    SmallFireball::new(id, position, direction, projectile::Owner::default());
                world.add_entity(Box::new(fireball));
                Some(DispenseOutcome::Launched {
                    sound: Some(level_event::SOUND_BLAZE_FIREBALL),
                })
            }
            Item::Tnt => {
                if !world.shared().rule_bool("tnt_explodes") {
                    return Some(DispenseOutcome::Failed);
                }
                let target = BlockPos::new(pos.x + sx, pos.y + sy, pos.z + sz);
                if let Some(outcome) = world.dispense_on_entity_in(target, stack) {
                    return Some(outcome);
                }
                world.spawn_primed_tnt(target, None, None);
                Some(DispenseOutcome::Dispensed)
            }
            _ => None,
        }
    }

    fn entity_melee_hit(
        &self,
        target: &mut dyn RegionEntity,
        attack: &EntityAttack,
        cx: &mut TickContext,
    ) {
        let kind = target.base().kind;
        let context = DamageContext::melee(kind);
        armor_post_attack(target, attack, &context, cx);
        let Some(weapon) = &attack.weapon else {
            return;
        };
        for effect in combat::post_attack(
            &[(EquipmentSlot::Mainhand, weapon)],
            Side::Attacker,
            &context,
            cx.rng,
        ) {
            if effect.affected != Side::Victim {
                continue;
            }
            match effect.effect {
                PostAttack::Ignite(seconds) => {
                    let base = target.base_mut();
                    let ticks = (seconds * 20.0).floor() as i16;
                    if !kind.size().fire_immune && base.fire < ticks {
                        base.fire = ticks;
                    }
                }
                PostAttack::ApplyMobEffect {
                    effect,
                    amplifier,
                    ticks,
                } => {
                    target.add_effects(&[ActiveEffect::timed(effect, i64::from(ticks), amplifier)]);
                }
                _ => {}
            }
        }
    }
}

/// The armor of a hit entity acting on the player who hit it (Thorns).
fn armor_post_attack(
    target: &dyn RegionEntity,
    attack: &EntityAttack,
    context: &DamageContext,
    cx: &mut TickContext,
) {
    let equipment = target.equipment();
    let worn: Vec<(EquipmentSlot, &ItemStack)> = equipment
        .iter()
        .filter_map(|(slot, stack)| {
            let slot = match slot {
                2 => EquipmentSlot::Feet,
                3 => EquipmentSlot::Legs,
                4 => EquipmentSlot::Chest,
                5 => EquipmentSlot::Head,
                _ => return None,
            };
            Some((slot, stack.as_ref()?.get()))
        })
        .collect();
    if worn.is_empty() {
        return;
    }
    for effect in combat::post_attack(&worn, Side::Victim, context, cx.rng) {
        if effect.affected != Side::Attacker {
            continue;
        }
        if let PostAttack::Damage { kind, amount } = effect.effect {
            let mut hit = EntityHit::new(amount, kind);
            hit.attacker = Some(Culprit::entity(target.base()));
            hit.position = Some(target.base().position);
            cx.effects.push(Effect::Hurt {
                target: attack.attacker,
                hit: Box::new(hit),
            });
        }
    }
}
