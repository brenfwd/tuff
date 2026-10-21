//! A mob's gear enchantment effects, run from their data: `tick` every
//! tick and `location_changed` when it changes block or gear.

use std::collections::BTreeSet;
use tuff_item::item::DupUnchecked;

use tuff_core::world::entity::TickContext;
use tuff_core::world::predicate::EntityFacts;
use tuff_item::item::{Enchantments, ItemStack};
use tuff_vanilla_common::enchant_effects::{
    self as fx, Action, ActiveKey, Body, EffectWorld, Executor, TickWorld,
};
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{Attribute, Item};
use tuff_world_model::BlockPos;

use super::{Damage, Mob, Species};

/// What the `location_changed` effects of a mob's gear last saw.
#[derive(Debug, Default)]
pub struct LocationEffects {
    block: Option<BlockPos>,
    active: BTreeSet<ActiveKey>,
    gear: Vec<(EquipmentSlot, Item, Option<Enchantments>)>,
    modifiers: Vec<(Attribute, String)>,
}

impl<S: Species> Mob<S> {
    pub(super) fn enchantment_effects(&mut self, cx: &mut TickContext) {
        let core = &self.core;
        let enchanted = core
            .equipment
            .worn()
            .iter()
            .any(|(_, stack)| stack.components.enchantments.is_some());
        if !enchanted && core.enchanted_location.is_none() {
            return;
        }
        let gear: Vec<(EquipmentSlot, ItemStack)> = core
            .equipment
            .worn()
            .into_iter()
            .map(|(slot, stack)| (slot, stack.dup_unchecked()))
            .collect();
        let worn: Vec<(EquipmentSlot, &ItemStack)> =
            gear.iter().map(|(slot, stack)| (*slot, stack)).collect();
        let facts = EntityFacts {
            kind: Some(core.base.kind),
            entity_id: Some(core.base.id),
            position: core.base.position,
            flags: tuff_core::world::predicate::Flags {
                on_fire: core.base.fire > 0,
                on_ground: core.base.on_ground,
                in_water: core.body.in_water,
                baby: core.is_baby(),
                ..Default::default()
            },
            equipment: gear.dup_unchecked(),
            tick_count: Some(cx.tick as i64),
            movement: Some(core.base.velocity),
            fall_distance: Some(core.base.fall_distance),
            ..EntityFacts::default()
        };
        let body = Body {
            kind: core.base.kind,
            position: core.base.position,
            movement: core.base.velocity,
        };
        let mut rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(rand::Rng::r#gen(
            &mut *cx.rng,
        ));
        let position = core.base.position;
        let block = BlockPos::new(
            position[0].floor() as i32,
            position[1].floor() as i32,
            position[2].floor() as i32,
        );
        let signature: Vec<(EquipmentSlot, Item, Option<Enchantments>)> = gear
            .iter()
            .map(|(slot, stack)| (*slot, stack.item, stack.components.enchantments.clone()))
            .collect();
        let mut fired = {
            let world = TickWorld { cx };
            let effects = EffectWorld::new(&mut rng).with_world(&world);
            fx::run(&worn, |e| &e.tick, &facts, &body, &effects)
        };
        let state = self
            .core
            .enchanted_location
            .get_or_insert_with(Box::default);
        let mut modifiers = None;
        if state.block != Some(block) || state.gear != signature {
            state.block = Some(block);
            state.gear = signature;
            let was_active = std::mem::take(&mut state.active);
            let change = {
                let world = TickWorld { cx };
                let mut effects = EffectWorld::new(&mut rng).with_world(&world);
                fx::location_changed(&worn, &facts, &body, &was_active, &mut effects)
            };
            let state = self
                .core
                .enchanted_location
                .get_or_insert_with(Box::default);
            state.active = change.active;
            fired.extend(change.fired);
            modifiers = Some(change.modifiers);
        }
        if let Some(modifiers) = modifiers {
            let state = self
                .core
                .enchanted_location
                .get_or_insert_with(Box::default);
            let old = std::mem::take(&mut state.modifiers);
            state.modifiers = modifiers
                .iter()
                .map(|(attribute, id, ..)| (*attribute, id.clone()))
                .collect();
            for (attribute, id) in old {
                self.core.put_modifier(attribute, id.into(), None, false);
            }
            for (attribute, id, amount, operation) in modifiers {
                self.core
                    .put_modifier(attribute, id.into(), Some((amount, operation)), false);
            }
        }
        if !enchanted
            && self
                .core
                .enchanted_location
                .as_ref()
                .is_some_and(|state| state.active.is_empty() && state.modifiers.is_empty())
        {
            self.core.enchanted_location = None;
        }
        for fired in fired {
            self.apply_enchantment_actions(fired.slot, fired.actions, cx);
        }
    }

    fn apply_enchantment_actions(
        &mut self,
        slot: EquipmentSlot,
        actions: Vec<Action>,
        cx: &mut TickContext,
    ) {
        let mut world = Vec::new();
        for action in actions {
            match action {
                Action::Ignite(seconds) => {
                    let ticks = (seconds * 20.0) as i16;
                    if !self.core.base.kind.size().fire_immune && self.core.base.fire < ticks {
                        self.core.base.fire = ticks;
                    }
                }
                Action::Damage { kind, amount } => {
                    self.hurt_by(&Damage::of(kind), amount, cx);
                }
                Action::ItemDamage(amount) => {
                    if let Some(mut stack) = self
                        .core
                        .equipment
                        .get(slot)
                        .map(DupUnchecked::dup_unchecked)
                    {
                        let rng = &mut *cx.rng;
                        if stack.hurt(amount, rng) == tuff_item::item::Hurt::Broken {
                            self.core.equipment.set(slot, None);
                        } else {
                            self.core.equipment.set(slot, Some(stack));
                        }
                    }
                }
                Action::MobEffect {
                    effect,
                    amplifier,
                    ticks,
                } => {
                    self.core
                        .add_effect(tuff_core::world::effects::ActiveEffect::timed(
                            effect,
                            i64::from(ticks),
                            amplifier,
                        ));
                }
                Action::Impulse {
                    direction,
                    scale,
                    magnitude,
                } => {
                    let push = fx::impulse(
                        direction,
                        scale,
                        magnitude,
                        self.core.base.yaw,
                        self.core.base.pitch,
                    );
                    for (v, p) in self.core.base.velocity.iter_mut().zip(push) {
                        *v += p;
                    }
                }
                Action::Exhaustion(_) => {}
                Action::World(action) => world.push(action),
            }
        }
        let executor = Executor {
            entity: self.core.base.id,
            uuid: Some(self.core.base.uuid),
            kind: self.core.base.kind,
            position: self.core.base.position,
        };
        fx::push_world(cx, executor, world);
    }
}
